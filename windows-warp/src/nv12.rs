//! BGRA to NV12 colour conversion on the GPU, with the Direct3D 11 video
//! processor. The encoder wants NV12 and the warp produces BGRA; converting
//! here keeps the frame on the GPU all the way into the encoder.

use anyhow::{anyhow, Result};
use windows::core::Interface;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::*;

/// Fields of `D3D11_VIDEO_PROCESSOR_COLOR_SPACE`: bit 0 usage, bit 1 RGB range,
/// bit 2 YCbCr matrix, bit 3 xvYCC, bits 4-5 nominal range.
const COLOR_SPACE_RGB_FULL: u32 = 2 << 4;
const COLOR_SPACE_YUV_BT709_STUDIO: u32 = (1 << 2) | (1 << 4);

/// Surfaces the conversion cycles through, so that the encoder can still be
/// reading a frame while the next one is converted.
const POOL_SIZE: usize = 4;

pub struct Nv12Converter {
    video_device: ID3D11VideoDevice,
    video_context: ID3D11VideoContext,
    processor: ID3D11VideoProcessor,
    enumerator: ID3D11VideoProcessorEnumerator,
    pool: Vec<(ID3D11Texture2D, ID3D11VideoProcessorOutputView)>,
    next: usize,
    input_view: Option<(ID3D11Texture2D, ID3D11VideoProcessorInputView)>,
}

impl Nv12Converter {
    pub fn new(
        device: &ID3D11Device,
        context: &ID3D11DeviceContext,
        width: u32,
        height: u32,
        fps: u32,
    ) -> Result<Self> {
        let video_device: ID3D11VideoDevice = device.cast()?;
        let video_context: ID3D11VideoContext = context.cast()?;

        let rate = DXGI_RATIONAL {
            Numerator: fps.max(1),
            Denominator: 1,
        };
        let content = D3D11_VIDEO_PROCESSOR_CONTENT_DESC {
            InputFrameFormat: D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
            InputFrameRate: rate,
            InputWidth: width,
            InputHeight: height,
            OutputFrameRate: rate,
            OutputWidth: width,
            OutputHeight: height,
            Usage: D3D11_VIDEO_USAGE_OPTIMAL_SPEED,
        };

        let enumerator = unsafe { video_device.CreateVideoProcessorEnumerator(&content)? };
        let processor = unsafe { video_device.CreateVideoProcessor(&enumerator, 0)? };

        let view_desc = D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC {
            ViewDimension: D3D11_VPOV_DIMENSION_TEXTURE2D,
            ..Default::default()
        };
        let mut pool = Vec::with_capacity(POOL_SIZE);
        for _ in 0..POOL_SIZE {
            let texture = create_nv12_texture(device, width, height)?;
            let mut view = None;
            unsafe {
                video_device.CreateVideoProcessorOutputView(
                    &texture,
                    &enumerator,
                    &view_desc,
                    Some(&mut view),
                )?;
            }
            pool.push((
                texture,
                view.ok_or_else(|| anyhow!("no video processor output view"))?,
            ));
        }

        unsafe {
            video_context.VideoProcessorSetStreamFrameFormat(
                &processor,
                0,
                D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
            );
            video_context.VideoProcessorSetStreamColorSpace(
                &processor,
                0,
                &D3D11_VIDEO_PROCESSOR_COLOR_SPACE {
                    _bitfield: COLOR_SPACE_RGB_FULL,
                },
            );
            video_context.VideoProcessorSetOutputColorSpace(
                &processor,
                &D3D11_VIDEO_PROCESSOR_COLOR_SPACE {
                    _bitfield: COLOR_SPACE_YUV_BT709_STUDIO,
                },
            );
        }

        Ok(Self {
            video_device,
            video_context,
            processor,
            enumerator,
            pool,
            next: 0,
            input_view: None,
        })
    }

    /// Converts `source` into the next NV12 surface of the pool and returns it.
    pub fn convert(&mut self, source: &ID3D11Texture2D) -> Result<ID3D11Texture2D> {
        let input_view = self.input_view_for(source)?;
        let (texture, output_view) = self.pool[self.next].clone();
        self.next = (self.next + 1) % self.pool.len();

        let stream = D3D11_VIDEO_PROCESSOR_STREAM {
            Enable: true.into(),
            OutputIndex: 0,
            InputFrameOrField: 0,
            pInputSurface: std::mem::ManuallyDrop::new(Some(input_view)),
            ..Default::default()
        };
        unsafe {
            self.video_context
                .VideoProcessorBlt(&self.processor, &output_view, 0, &[stream])?;
        }
        Ok(texture)
    }

    fn input_view_for(
        &mut self,
        source: &ID3D11Texture2D,
    ) -> Result<ID3D11VideoProcessorInputView> {
        if let Some((texture, view)) = &self.input_view {
            if texture == source {
                return Ok(view.clone());
            }
        }

        let desc = D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC {
            FourCC: 0,
            ViewDimension: D3D11_VPIV_DIMENSION_TEXTURE2D,
            ..Default::default()
        };
        let mut view = None;
        unsafe {
            self.video_device.CreateVideoProcessorInputView(
                source,
                &self.enumerator,
                &desc,
                Some(&mut view),
            )?;
        }
        let view = view.ok_or_else(|| anyhow!("no video processor input view"))?;
        self.input_view = Some((source.clone(), view.clone()));
        Ok(view)
    }
}

fn create_nv12_texture(device: &ID3D11Device, width: u32, height: u32) -> Result<ID3D11Texture2D> {
    let desc = D3D11_TEXTURE2D_DESC {
        Width: width,
        Height: height,
        MipLevels: 1,
        ArraySize: 1,
        Format: DXGI_FORMAT_NV12,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        Usage: D3D11_USAGE_DEFAULT,
        BindFlags: (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32,
        ..Default::default()
    };
    let mut texture = None;
    unsafe { device.CreateTexture2D(&desc, None, Some(&mut texture))? };
    texture.ok_or_else(|| anyhow!("failed to create the NV12 texture"))
}
