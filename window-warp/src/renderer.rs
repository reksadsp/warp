use anyhow::{anyhow, Context, Result};
use windows::core::{Interface, PCSTR};
use windows::Win32::Foundation::{HMODULE, HWND};
use windows::Win32::Graphics::Direct3D::Fxc::{
    D3DCompile, D3DCOMPILE_ENABLE_STRICTNESS, D3DCOMPILE_OPTIMIZATION_LEVEL3,
};
use windows::Win32::Graphics::Direct3D::{ID3DBlob, D3D_DRIVER_TYPE_HARDWARE};
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Dxgi::*;

use crate::capture::CapturedFrame;
use crate::cli::WarpParams;

const SHADER_SOURCE: &str = include_str!("shaders/warp.hlsl");

#[repr(C)]
#[derive(Clone, Copy)]
struct WarpConstants {
    output_size: [f32; 2],
    inner_radius: f32,
    outer_radius: f32,
    start_angle: f32,
    angle_span: f32,
    direction: f32,
    supersample: u32,
    background: [f32; 4],
}

struct SourceTexture {
    texture: ID3D11Texture2D,
    view: ID3D11ShaderResourceView,
    width: u32,
    height: u32,
}

pub struct Renderer {
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    swap_chain: IDXGISwapChain1,
    target_view: Option<ID3D11RenderTargetView>,
    vertex_shader: ID3D11VertexShader,
    pixel_shader: ID3D11PixelShader,
    sampler: ID3D11SamplerState,
    constants: ID3D11Buffer,
    source: Option<SourceTexture>,
    width: u32,
    height: u32,
}

impl Renderer {
    pub fn new(hwnd: HWND, width: u32, height: u32) -> Result<Self> {
        let (device, context) = create_device()?;
        let swap_chain = create_swap_chain(&device, hwnd, width, height)?;

        let vs_blob = compile_shader("vs_main", "vs_5_0")?;
        let ps_blob = compile_shader("ps_main", "ps_5_0")?;

        let mut vertex_shader = None;
        let mut pixel_shader = None;
        unsafe {
            device.CreateVertexShader(blob_bytes(&vs_blob), None, Some(&mut vertex_shader))?;
            device.CreatePixelShader(blob_bytes(&ps_blob), None, Some(&mut pixel_shader))?;
        }

        let sampler_desc = D3D11_SAMPLER_DESC {
            Filter: D3D11_FILTER_MIN_MAG_MIP_LINEAR,
            // The angle axis wraps around the disk, the radius axis does not.
            AddressU: D3D11_TEXTURE_ADDRESS_WRAP,
            AddressV: D3D11_TEXTURE_ADDRESS_CLAMP,
            AddressW: D3D11_TEXTURE_ADDRESS_CLAMP,
            ComparisonFunc: D3D11_COMPARISON_NEVER,
            MaxLOD: f32::MAX,
            ..Default::default()
        };
        let mut sampler = None;
        unsafe { device.CreateSamplerState(&sampler_desc, Some(&mut sampler))? };

        let constants_desc = D3D11_BUFFER_DESC {
            ByteWidth: std::mem::size_of::<WarpConstants>() as u32,
            Usage: D3D11_USAGE_DYNAMIC,
            BindFlags: D3D11_BIND_CONSTANT_BUFFER.0 as u32,
            CPUAccessFlags: D3D11_CPU_ACCESS_WRITE.0 as u32,
            ..Default::default()
        };
        let mut constants = None;
        unsafe { device.CreateBuffer(&constants_desc, None, Some(&mut constants))? };

        let mut renderer = Self {
            device,
            context,
            swap_chain,
            target_view: None,
            vertex_shader: vertex_shader.ok_or_else(|| anyhow!("no vertex shader"))?,
            pixel_shader: pixel_shader.ok_or_else(|| anyhow!("no pixel shader"))?,
            sampler: sampler.ok_or_else(|| anyhow!("no sampler state"))?,
            constants: constants.ok_or_else(|| anyhow!("no constant buffer"))?,
            source: None,
            width,
            height,
        };
        renderer.create_target_view()?;
        Ok(renderer)
    }

    fn create_target_view(&mut self) -> Result<()> {
        unsafe {
            let back_buffer: ID3D11Texture2D = self.swap_chain.GetBuffer(0)?;
            let mut view = None;
            self.device
                .CreateRenderTargetView(&back_buffer, None, Some(&mut view))?;
            self.target_view = view;
        }
        Ok(())
    }

    pub fn resize(&mut self, width: u32, height: u32) -> Result<()> {
        if width == 0 || height == 0 || (width == self.width && height == self.height) {
            return Ok(());
        }
        self.width = width;
        self.height = height;
        unsafe {
            self.context.OMSetRenderTargets(None, None);
            self.target_view = None;
            self.swap_chain.ResizeBuffers(
                0,
                width,
                height,
                DXGI_FORMAT_UNKNOWN,
                DXGI_SWAP_CHAIN_FLAG(0),
            )?;
        }
        self.create_target_view()
    }

    /// Uploads a captured frame into the texture sampled by the warp shader.
    pub fn upload(&mut self, frame: &CapturedFrame) -> Result<()> {
        let expected = frame.width as usize * frame.height as usize * 4;
        if frame.bgra.len() < expected {
            return Err(anyhow!(
                "frame buffer is {} bytes, expected at least {expected}",
                frame.bgra.len()
            ));
        }

        let needs_new_texture = match &self.source {
            Some(source) => source.width != frame.width || source.height != frame.height,
            None => true,
        };
        if needs_new_texture {
            self.source = Some(self.create_source_texture(frame.width, frame.height)?);
        }
        let source = self.source.as_ref().expect("source texture just created");

        unsafe {
            let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
            self.context.Map(
                &source.texture,
                0,
                D3D11_MAP_WRITE_DISCARD,
                0,
                Some(&mut mapped),
            )?;

            let src_pitch = frame.width as usize * 4;
            let dst_pitch = mapped.RowPitch as usize;
            for row in 0..frame.height as usize {
                let src = frame.bgra[row * src_pitch..][..src_pitch].as_ptr();
                let dst = (mapped.pData as *mut u8).add(row * dst_pitch);
                std::ptr::copy_nonoverlapping(src, dst, src_pitch);
            }

            self.context.Unmap(&source.texture, 0);
        }
        Ok(())
    }

    fn create_source_texture(&self, width: u32, height: u32) -> Result<SourceTexture> {
        let desc = D3D11_TEXTURE2D_DESC {
            Width: width,
            Height: height,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: D3D11_USAGE_DYNAMIC,
            BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
            CPUAccessFlags: D3D11_CPU_ACCESS_WRITE.0 as u32,
            MiscFlags: 0,
        };

        unsafe {
            let mut texture = None;
            self.device
                .CreateTexture2D(&desc, None, Some(&mut texture))?;
            let texture = texture.ok_or_else(|| anyhow!("no source texture"))?;

            let mut view = None;
            self.device
                .CreateShaderResourceView(&texture, None, Some(&mut view))?;

            Ok(SourceTexture {
                texture,
                view: view.ok_or_else(|| anyhow!("no shader resource view"))?,
                width,
                height,
            })
        }
    }

    pub fn render(&mut self, params: &WarpParams) -> Result<()> {
        let Some(target_view) = self.target_view.clone() else {
            return Ok(());
        };
        let source_view = self.source.as_ref().map(|source| source.view.clone());

        self.update_constants(params)?;

        unsafe {
            let viewport = D3D11_VIEWPORT {
                TopLeftX: 0.0,
                TopLeftY: 0.0,
                Width: self.width as f32,
                Height: self.height as f32,
                MinDepth: 0.0,
                MaxDepth: 1.0,
            };
            self.context.RSSetViewports(Some(&[viewport]));
            self.context
                .OMSetRenderTargets(Some(&[Some(target_view.clone())]), None);
            self.context
                .ClearRenderTargetView(&target_view, &[0.0, 0.0, 0.0, 1.0]);

            if let Some(source_view) = source_view {
                self.context.IASetPrimitiveTopology(
                    windows::Win32::Graphics::Direct3D::D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST,
                );
                self.context.VSSetShader(&self.vertex_shader, None);
                self.context.PSSetShader(&self.pixel_shader, None);
                self.context
                    .PSSetConstantBuffers(0, Some(&[Some(self.constants.clone())]));
                self.context
                    .PSSetShaderResources(0, Some(&[Some(source_view)]));
                self.context
                    .PSSetSamplers(0, Some(&[Some(self.sampler.clone())]));
                self.context.Draw(3, 0);
            }

            self.swap_chain.Present(1, DXGI_PRESENT(0)).ok()?;
        }
        Ok(())
    }

    fn update_constants(&self, params: &WarpParams) -> Result<()> {
        let constants = WarpConstants {
            output_size: [self.width as f32, self.height as f32],
            inner_radius: params.inner_radius,
            outer_radius: params.outer_radius,
            start_angle: params.start_angle_deg.to_radians(),
            angle_span: params.angle_span_deg.to_radians(),
            direction: if params.clockwise { 1.0 } else { -1.0 },
            supersample: params.supersample,
            background: [0.0, 0.0, 0.0, 1.0],
        };

        unsafe {
            let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
            self.context.Map(
                &self.constants,
                0,
                D3D11_MAP_WRITE_DISCARD,
                0,
                Some(&mut mapped),
            )?;
            std::ptr::copy_nonoverlapping(
                &constants as *const WarpConstants,
                mapped.pData as *mut WarpConstants,
                1,
            );
            self.context.Unmap(&self.constants, 0);
        }
        Ok(())
    }
}

fn create_device() -> Result<(ID3D11Device, ID3D11DeviceContext)> {
    let mut device = None;
    let mut context = None;
    unsafe {
        D3D11CreateDevice(
            None,
            D3D_DRIVER_TYPE_HARDWARE,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            None,
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            Some(&mut context),
        )
        .context("failed to create the Direct3D 11 device")?;
    }
    Ok((
        device.ok_or_else(|| anyhow!("no D3D11 device"))?,
        context.ok_or_else(|| anyhow!("no D3D11 device context"))?,
    ))
}

fn create_swap_chain(
    device: &ID3D11Device,
    hwnd: HWND,
    width: u32,
    height: u32,
) -> Result<IDXGISwapChain1> {
    let desc = DXGI_SWAP_CHAIN_DESC1 {
        Width: width,
        Height: height,
        Format: DXGI_FORMAT_B8G8R8A8_UNORM,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
        BufferCount: 2,
        Scaling: DXGI_SCALING_STRETCH,
        SwapEffect: DXGI_SWAP_EFFECT_FLIP_DISCARD,
        AlphaMode: DXGI_ALPHA_MODE_IGNORE,
        ..Default::default()
    };

    unsafe {
        let dxgi_device: IDXGIDevice = device.cast()?;
        let adapter = dxgi_device.GetAdapter()?;
        let factory: IDXGIFactory2 = adapter.GetParent()?;
        Ok(factory.CreateSwapChainForHwnd(device, hwnd, &desc, None, None)?)
    }
}

fn compile_shader(entry: &str, target: &str) -> Result<ID3DBlob> {
    let entry = std::ffi::CString::new(entry)?;
    let target = std::ffi::CString::new(target)?;
    let name = std::ffi::CString::new("warp.hlsl")?;

    let mut code = None;
    let mut errors = None;
    let result = unsafe {
        D3DCompile(
            SHADER_SOURCE.as_ptr() as *const std::ffi::c_void,
            SHADER_SOURCE.len(),
            PCSTR(name.as_ptr() as *const u8),
            None,
            None,
            PCSTR(entry.as_ptr() as *const u8),
            PCSTR(target.as_ptr() as *const u8),
            D3DCOMPILE_ENABLE_STRICTNESS | D3DCOMPILE_OPTIMIZATION_LEVEL3,
            0,
            &mut code,
            Some(&mut errors),
        )
    };

    if let Err(error) = result {
        let message = errors
            .map(|errors| unsafe {
                let bytes = std::slice::from_raw_parts(
                    errors.GetBufferPointer() as *const u8,
                    errors.GetBufferSize(),
                );
                String::from_utf8_lossy(bytes).into_owned()
            })
            .unwrap_or_else(|| error.to_string());
        return Err(anyhow!(
            "failed to compile {}: {message}",
            target.to_string_lossy()
        ));
    }

    code.ok_or_else(|| anyhow!("shader compiler returned no bytecode"))
}

fn blob_bytes(blob: &ID3DBlob) -> &[u8] {
    unsafe {
        std::slice::from_raw_parts(blob.GetBufferPointer() as *const u8, blob.GetBufferSize())
    }
}
