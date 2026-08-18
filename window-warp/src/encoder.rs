//! H.264 encoding of the warped frames with a Media Foundation transform.
//!
//! The BGRA render target never leaves the GPU: it is converted to NV12 by the
//! Direct3D video processor and handed to the encoder as a DXGI surface. Only
//! the compressed bitstream is copied back to system memory, which is what the
//! network sink needs.

use anyhow::{anyhow, bail, Context, Result};
use windows::core::Interface;
use windows::Win32::Graphics::Direct3D11::{ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D};
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::System::Variant::{VARIANT, VT_UI4};

use crate::nv12::Nv12Converter;

/// Media Foundation counts time in 100 ns units.
const UNITS_PER_SECOND: i64 = 10_000_000;

#[derive(Debug, Clone, Copy)]
pub struct EncoderConfig {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    /// Average bitrate, in bits per second.
    pub bitrate: u32,
    /// Distance between key frames, in frames.
    pub gop: u32,
}

/// One coded frame, in Annex B form (NAL units prefixed with start codes).
pub struct EncodedFrame {
    pub data: Vec<u8>,
    pub time_100ns: i64,
    pub keyframe: bool,
}

impl EncodedFrame {
    /// Presentation time on the 90 kHz clock transport streams are timed with.
    pub fn pts_90khz(&self) -> u64 {
        (self.time_100ns.max(0) as u64) * 9 / 1000
    }
}

pub struct H264Encoder {
    transform: IMFTransform,
    events: Option<IMFMediaEventGenerator>,
    converter: Nv12Converter,
    input_id: u32,
    output_id: u32,
    provides_samples: bool,
    output_buffer_size: u32,
    sequence_header: Vec<u8>,
    frame_duration: i64,
    frame_index: i64,
    _device_manager: IMFDXGIDeviceManager,
}

impl H264Encoder {
    pub fn new(
        device: &ID3D11Device,
        context: &ID3D11DeviceContext,
        config: EncoderConfig,
    ) -> Result<Self> {
        unsafe {
            MFStartup(MF_VERSION, MFSTARTUP_LITE).context("failed to start MediaFoundation")?
        };

        let converter =
            Nv12Converter::new(device, context, config.width, config.height, config.fps)
                .context("failed to set up the BGRA to NV12 conversion")?;

        // The encoder runs on its own threads, so the device has to be safe to
        // use from several of them.
        let multithread: windows::Win32::Graphics::Direct3D11::ID3D11Multithread =
            context.cast()?;
        unsafe {
            let _ = multithread.SetMultithreadProtected(true);
        };

        let mut token = 0u32;
        let mut device_manager = None;
        unsafe { MFCreateDXGIDeviceManager(&mut token, &mut device_manager)? };
        let device_manager = device_manager.ok_or_else(|| anyhow!("no DXGI device manager"))?;
        unsafe { device_manager.ResetDevice(device, token)? };

        let (transform, hardware) = find_encoder()?;
        let attributes = unsafe { transform.GetAttributes() }.ok();
        let asynchronous = attributes.as_ref().is_some_and(|attributes| {
            unsafe { attributes.GetUINT32(&MF_TRANSFORM_ASYNC) }.unwrap_or(0) == 1
        });
        if asynchronous {
            let attributes = attributes
                .as_ref()
                .ok_or_else(|| anyhow!("asynchronous transform without attributes"))?;
            unsafe { attributes.SetUINT32(&MF_TRANSFORM_ASYNC_UNLOCK, 1)? };
        }

        if hardware {
            unsafe {
                transform.ProcessMessage(
                    MFT_MESSAGE_SET_D3D_MANAGER,
                    device_manager.as_raw() as usize,
                )?;
            }
        }

        let (input_id, output_id) = stream_ids(&transform);
        configure_types(&transform, input_id, output_id, &config)?;
        configure_codec(&transform, &config);

        let stream_info = unsafe { transform.GetOutputStreamInfo(output_id)? };
        let provides_samples =
            stream_info.dwFlags & MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 as u32 != 0;

        let sequence_header = unsafe {
            transform
                .GetOutputCurrentType(output_id)
                .ok()
                .and_then(|media_type| read_blob(&media_type, &MF_MT_MPEG_SEQUENCE_HEADER))
                .unwrap_or_default()
        };

        let events: Option<IMFMediaEventGenerator> = if asynchronous {
            Some(transform.cast()?)
        } else {
            None
        };

        unsafe {
            transform.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)?;
            transform.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)?;
        }

        Ok(Self {
            transform,
            events,
            converter,
            input_id,
            output_id,
            provides_samples,
            output_buffer_size: stream_info.cbSize.max(1),
            sequence_header,
            frame_duration: UNITS_PER_SECOND / config.fps.max(1) as i64,
            frame_index: 0,
            _device_manager: device_manager,
        })
    }

    /// Encodes one warped BGRA frame and appends every coded frame that came
    /// out of the encoder to `out`.
    pub fn encode(&mut self, bgra: &ID3D11Texture2D, out: &mut Vec<EncodedFrame>) -> Result<()> {
        let nv12 = self.converter.convert(bgra)?;
        let sample = self.wrap_surface(&nv12)?;

        if self.events.is_some() {
            self.feed_asynchronous(&sample, out)?;
        } else {
            unsafe { self.transform.ProcessInput(self.input_id, &sample, 0)? };
            while self.collect_output(out)? {}
        }
        Ok(())
    }

    /// Flushes the frames the encoder is still holding, at the end of a stream.
    pub fn drain(&mut self, out: &mut Vec<EncodedFrame>) -> Result<()> {
        unsafe {
            self.transform
                .ProcessMessage(MFT_MESSAGE_COMMAND_DRAIN, 0)?
        };
        if self.events.is_some() {
            self.drain_events(true, out)?;
        } else {
            while self.collect_output(out)? {}
        }
        Ok(())
    }

    fn wrap_surface(&mut self, surface: &ID3D11Texture2D) -> Result<IMFSample> {
        unsafe {
            let buffer = MFCreateDXGISurfaceBuffer(&ID3D11Texture2D::IID, surface, 0, false)?;
            let length = buffer.cast::<IMF2DBuffer>()?.GetContiguousLength()?;
            buffer.SetCurrentLength(length)?;

            let sample = MFCreateSample()?;
            sample.AddBuffer(&buffer)?;
            sample.SetSampleTime(self.frame_index * self.frame_duration)?;
            sample.SetSampleDuration(self.frame_duration)?;
            self.frame_index += 1;
            Ok(sample)
        }
    }

    /// Asynchronous transforms ask for input and announce output through
    /// events, so a frame can only be submitted once the encoder asked for one.
    fn feed_asynchronous(&mut self, sample: &IMFSample, out: &mut Vec<EncodedFrame>) -> Result<()> {
        loop {
            let event = unsafe {
                self.event_generator()?
                    .GetEvent(MEDIA_EVENT_GENERATOR_GET_EVENT_FLAGS(0))?
            };
            match unsafe { event.GetType()? } as i32 {
                event if event == METransformNeedInput.0 => {
                    unsafe { self.transform.ProcessInput(self.input_id, sample, 0)? };
                    break;
                }
                event if event == METransformHaveOutput.0 => {
                    self.collect_output(out)?;
                }
                _ => {}
            }
        }
        // Whatever the encoder finished in the meantime, without waiting.
        self.drain_events(false, out)
    }

    fn drain_events(&mut self, blocking: bool, out: &mut Vec<EncodedFrame>) -> Result<()> {
        let flags = if blocking {
            MEDIA_EVENT_GENERATOR_GET_EVENT_FLAGS(0)
        } else {
            MF_EVENT_FLAG_NO_WAIT
        };
        loop {
            let event = match unsafe { self.event_generator()?.GetEvent(flags) } {
                Ok(event) => event,
                Err(error) if error.code() == MF_E_NO_EVENTS_AVAILABLE => return Ok(()),
                Err(error) if error.code() == MF_E_SHUTDOWN => return Ok(()),
                Err(error) => return Err(error.into()),
            };
            match unsafe { event.GetType()? } as i32 {
                event if event == METransformHaveOutput.0 => self.collect_output(out)?,
                event if event == METransformDrainComplete.0 => return Ok(()),
                _ => continue,
            };
        }
    }

    fn event_generator(&self) -> Result<&IMFMediaEventGenerator> {
        self.events
            .as_ref()
            .ok_or_else(|| anyhow!("the transform is not asynchronous"))
    }

    /// Pulls one coded frame out of the transform. Returns whether it produced
    /// one, so that synchronous transforms can be drained in a loop.
    fn collect_output(&mut self, out: &mut Vec<EncodedFrame>) -> Result<bool> {
        let mut buffer = MFT_OUTPUT_DATA_BUFFER {
            dwStreamID: self.output_id,
            ..Default::default()
        };
        if !self.provides_samples {
            unsafe {
                let sample = MFCreateSample()?;
                sample.AddBuffer(&MFCreateMemoryBuffer(self.output_buffer_size)?)?;
                buffer.pSample = std::mem::ManuallyDrop::new(Some(sample));
            }
        }

        let mut status = 0u32;
        let result = unsafe {
            self.transform
                .ProcessOutput(0, std::slice::from_mut(&mut buffer), &mut status)
        };
        let sample = unsafe { std::mem::ManuallyDrop::take(&mut buffer.pSample) };
        unsafe { std::mem::ManuallyDrop::drop(&mut buffer.pEvents) };

        match result {
            Ok(()) => {}
            Err(error) if error.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => return Ok(false),
            Err(error) if error.code() == MF_E_TRANSFORM_STREAM_CHANGE => {
                // The encoder settled on a different output type: accept it and
                // pick up the new sequence header.
                unsafe {
                    let media_type = self.transform.GetOutputAvailableType(self.output_id, 0)?;
                    self.transform
                        .SetOutputType(self.output_id, &media_type, 0)?;
                    if let Some(header) = read_blob(&media_type, &MF_MT_MPEG_SEQUENCE_HEADER) {
                        self.sequence_header = header;
                    }
                }
                return Ok(false);
            }
            Err(error) => return Err(error.into()),
        }

        let Some(sample) = sample else {
            return Ok(false);
        };
        out.push(self.read_sample(&sample)?);
        Ok(true)
    }

    fn read_sample(&self, sample: &IMFSample) -> Result<EncodedFrame> {
        unsafe {
            let time_100ns = sample.GetSampleTime().unwrap_or(0);
            let keyframe = sample.GetUINT32(&MFSampleExtension_CleanPoint).unwrap_or(0) == 1;

            let buffer = sample.ConvertToContiguousBuffer()?;
            let mut pointer = std::ptr::null_mut();
            let mut length = 0u32;
            buffer.Lock(&mut pointer, None, Some(&mut length))?;
            let mut data = Vec::with_capacity(length as usize);
            data.extend_from_slice(std::slice::from_raw_parts(pointer, length as usize));
            buffer.Unlock()?;

            // Some encoders only send the parameter sets out of band, but a
            // player joining a live stream needs them before every key frame.
            if keyframe && !self.sequence_header.is_empty() && !has_parameter_sets(&data) {
                let mut with_header = self.sequence_header.clone();
                with_header.extend_from_slice(&data);
                data = with_header;
            }

            Ok(EncodedFrame {
                data,
                time_100ns,
                keyframe,
            })
        }
    }
}

impl Drop for H264Encoder {
    fn drop(&mut self) {
        unsafe {
            let _ = self
                .transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_END_STREAMING, 0);
            let _ = MFShutdown();
        }
    }
}

/// Picks the first H.264 encoder that Media Foundation offers, hardware first.
fn find_encoder() -> Result<(IMFTransform, bool)> {
    let output = MFT_REGISTER_TYPE_INFO {
        guidMajorType: MFMediaType_Video,
        guidSubtype: MFVideoFormat_H264,
    };
    let input = MFT_REGISTER_TYPE_INFO {
        guidMajorType: MFMediaType_Video,
        guidSubtype: MFVideoFormat_NV12,
    };

    for (flags, hardware) in [
        (
            MFT_ENUM_FLAG_HARDWARE | MFT_ENUM_FLAG_ASYNCMFT | MFT_ENUM_FLAG_SORTANDFILTER,
            true,
        ),
        (MFT_ENUM_FLAG_SYNCMFT | MFT_ENUM_FLAG_SORTANDFILTER, false),
    ] {
        let mut activates = std::ptr::null_mut();
        let mut count = 0u32;
        unsafe {
            MFTEnumEx(
                MFT_CATEGORY_VIDEO_ENCODER,
                flags,
                Some(&input),
                Some(&output),
                &mut activates,
                &mut count,
            )?;
        }

        let mut transform = None;
        for index in 0..count as usize {
            // Takes ownership of the entry so that the ones that are not
            // activated are released.
            let activate: Option<IMFActivate> = unsafe { std::ptr::read(activates.add(index)) };
            if transform.is_none() {
                if let Some(activate) = activate {
                    transform = unsafe { activate.ActivateObject::<IMFTransform>() }.ok();
                }
            }
        }
        unsafe { CoTaskMemFree(Some(activates as *const _)) };

        if let Some(transform) = transform {
            return Ok((transform, hardware));
        }
    }

    bail!("no H.264 encoder is available on this machine")
}

fn stream_ids(transform: &IMFTransform) -> (u32, u32) {
    let mut input = [0u32; 1];
    let mut output = [0u32; 1];
    match unsafe { transform.GetStreamIDs(&mut input, &mut output) } {
        Ok(()) => (input[0], output[0]),
        // E_NOTIMPL means the streams are simply numbered from zero.
        Err(_) => (0, 0),
    }
}

fn configure_types(
    transform: &IMFTransform,
    input_id: u32,
    output_id: u32,
    config: &EncoderConfig,
) -> Result<()> {
    unsafe {
        // Encoders want their output type before their input type.
        let output_type = MFCreateMediaType()?;
        output_type.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
        output_type.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_H264)?;
        output_type.SetUINT32(&MF_MT_AVG_BITRATE, config.bitrate)?;
        output_type.SetUINT64(&MF_MT_FRAME_SIZE, pack(config.width, config.height))?;
        output_type.SetUINT64(&MF_MT_FRAME_RATE, pack(config.fps, 1))?;
        output_type.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, pack(1, 1))?;
        output_type.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
        output_type.SetUINT32(&MF_MT_MPEG2_PROFILE, eAVEncH264VProfile_High.0 as u32)?;
        transform
            .SetOutputType(output_id, &output_type, 0)
            .context("the encoder rejected the H.264 output type")?;

        let input_type = MFCreateMediaType()?;
        input_type.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
        input_type.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_NV12)?;
        input_type.SetUINT64(&MF_MT_FRAME_SIZE, pack(config.width, config.height))?;
        input_type.SetUINT64(&MF_MT_FRAME_RATE, pack(config.fps, 1))?;
        input_type.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, pack(1, 1))?;
        input_type.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
        transform
            .SetInputType(input_id, &input_type, 0)
            .context("the encoder rejected the NV12 input type")?;
    }
    Ok(())
}

/// Rate control, key frame distance and latency. Encoders may not support all
/// of them, and the defaults are reasonable, so failures are not fatal.
fn configure_codec(transform: &IMFTransform, config: &EncoderConfig) {
    let Ok(codec) = transform.cast::<ICodecAPI>() else {
        return;
    };
    unsafe {
        let _ = codec.SetValue(
            &CODECAPI_AVEncCommonRateControlMode,
            &variant_u32(eAVEncCommonRateControlMode_CBR.0 as u32),
        );
        let _ = codec.SetValue(
            &CODECAPI_AVEncCommonMeanBitRate,
            &variant_u32(config.bitrate),
        );
        let _ = codec.SetValue(&CODECAPI_AVEncMPVGOPSize, &variant_u32(config.gop));
        let _ = codec.SetValue(&CODECAPI_AVLowLatencyMode, &variant_u32(1));
    }
}

fn variant_u32(value: u32) -> VARIANT {
    let mut variant = VARIANT::default();
    unsafe {
        let inner = &mut variant.Anonymous.Anonymous;
        inner.vt = VT_UI4;
        inner.Anonymous.ulVal = value;
    }
    variant
}

/// Media Foundation stores pairs of 32 bit values, sizes and ratios, in a
/// single 64 bit attribute.
fn pack(high: u32, low: u32) -> u64 {
    ((high as u64) << 32) | low as u64
}

fn read_blob(media_type: &IMFMediaType, key: &windows::core::GUID) -> Option<Vec<u8>> {
    unsafe {
        let size = media_type.GetBlobSize(key).ok()? as usize;
        let mut blob = vec![0u8; size];
        let mut written = 0u32;
        media_type
            .GetBlob(key, &mut blob, Some(&mut written))
            .ok()?;
        blob.truncate(written as usize);
        Some(blob)
    }
}

/// Whether an access unit already carries a sequence parameter set.
fn has_parameter_sets(data: &[u8]) -> bool {
    nal_types(data).any(|nal_type| nal_type == 7)
}

/// NAL unit types of an Annex B access unit.
fn nal_types(data: &[u8]) -> impl Iterator<Item = u8> + '_ {
    data.windows(4).filter_map(|window| {
        let start_code = window[0] == 0 && window[1] == 0 && window[2] == 1;
        start_code.then_some(window[3] & 0x1f)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_a_sequence_parameter_set() {
        let sps = [0, 0, 0, 1, 0x67, 0x42, 0, 0, 0, 1, 0x68, 0xce];
        let slice = [0, 0, 0, 1, 0x65, 0x88, 0x84];
        assert!(has_parameter_sets(&sps));
        assert!(!has_parameter_sets(&slice));
    }

    #[test]
    fn packs_pairs_into_one_attribute() {
        assert_eq!(pack(1920, 1080), 0x0000_0780_0000_0438);
    }
}
