use std::mem;

use color_eyre::eyre;
use windows::core::{s, Interface};
use windows::Win32::Graphics::Direct3D::Fxc::{D3DCompile, D3DCOMPILE_ENABLE_STRICTNESS};
use windows::Win32::Graphics::Direct3D::{ID3DInclude, D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST};
use windows::Win32::Graphics::Direct3D11::{
    ID3D11Buffer, ID3D11Device, ID3D11DeviceContext, ID3D11InputLayout, ID3D11PixelShader,
    ID3D11SamplerState, ID3D11ShaderResourceView, ID3D11Texture2D, ID3D11VertexShader,
    D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE, D3D11_BIND_VERTEX_BUFFER,
    D3D11_BUFFER_DESC, D3D11_COMPARISON_NEVER, D3D11_CPU_ACCESS_FLAG,
    D3D11_FILTER_MIN_MAG_MIP_POINT, D3D11_FLOAT32_MAX, D3D11_INPUT_ELEMENT_DESC,
    D3D11_INPUT_PER_VERTEX_DATA, D3D11_RESOURCE_MISC_SHARED, D3D11_SAMPLER_DESC,
    D3D11_SUBRESOURCE_DATA, D3D11_TEXTURE2D_DESC, D3D11_TEXTURE_ADDRESS_CLAMP, D3D11_USAGE_DEFAULT,
    D3D11_USAGE_IMMUTABLE, D3D11_VIEWPORT,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_R32G32B32_FLOAT, DXGI_FORMAT_R32G32_FLOAT,
    DXGI_SAMPLE_DESC,
};
use windows::Win32::Graphics::Hlsl::D3D_COMPILE_STANDARD_FILE_INCLUDE;

pub struct Renderer {
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    vertex_shader: ID3D11VertexShader,
    pixel_shader: ID3D11PixelShader,
    vertex_buffer: ID3D11Buffer,
    input_layout: ID3D11InputLayout,
    sampler_state: ID3D11SamplerState,
}

#[repr(C)]
struct Vertex(f32, f32, f32, f32, f32);

const _: () = {
    assert!(mem::size_of::<Vertex>() == 5 * 4);
};

static VERTEX_DATA: [Vertex; 6] = [
    // Top left triangle
    Vertex(-1.0, 1.0, 0.0, 0.0, 0.0),
    Vertex(1.0, 1.0, 0.0, 1.0, 0.0),
    Vertex(-1.0, -1.0, 0.0, 0.0, 1.0),
    // Bottom right triangle
    Vertex(1.0, 1.0, 0.0, 1.0, 0.0),
    Vertex(1.0, -1.0, 0.0, 1.0, 1.0),
    Vertex(-1.0, -1.0, 0.0, 0.0, 1.0),
];

impl Renderer {
    pub fn new(device: ID3D11Device) -> eyre::Result<Renderer> {
        let shader_source = include_bytes!("shaders.hlsl");

        let (vs_blob, ps_blob) = unsafe {
            let mut vs_blob = None;
            let mut ps_blob = None;

            D3DCompile(
                shader_source.as_ptr().cast(),
                shader_source.len(),
                s!("shaders.hlsl"),
                None,
                &ID3DInclude::from_raw(D3D_COMPILE_STANDARD_FILE_INCLUDE as _),
                s!("vs_main"),
                s!("vs_5_0"),
                D3DCOMPILE_ENABLE_STRICTNESS,
                0,
                &mut vs_blob,
                None,
            )?;

            D3DCompile(
                shader_source.as_ptr().cast(),
                shader_source.len(),
                s!("shaders.hlsl"),
                None,
                &ID3DInclude::from_raw(D3D_COMPILE_STANDARD_FILE_INCLUDE as _),
                s!("ps_main"),
                s!("ps_5_0"),
                D3DCOMPILE_ENABLE_STRICTNESS,
                0,
                &mut ps_blob,
                None,
            )?;

            (vs_blob.unwrap(), ps_blob.unwrap())
        };

        let vertex_shader_bytes = unsafe {
            std::slice::from_raw_parts(
                vs_blob.GetBufferPointer().cast::<u8>(),
                vs_blob.GetBufferSize(),
            )
        };

        let pixel_shader_bytes = unsafe {
            std::slice::from_raw_parts(
                ps_blob.GetBufferPointer().cast::<u8>(),
                ps_blob.GetBufferSize(),
            )
        };

        let (vertex_shader, pixel_shader) = unsafe {
            let mut vs = None;
            let mut ps = None;

            device.CreateVertexShader(vertex_shader_bytes, None, Some(&mut vs))?;

            device.CreatePixelShader(pixel_shader_bytes, None, Some(&mut ps))?;

            (vs.unwrap(), ps.unwrap())
        };

        let input_desc = [
            D3D11_INPUT_ELEMENT_DESC {
                SemanticName: s!("POS"),
                SemanticIndex: 0,
                Format: DXGI_FORMAT_R32G32B32_FLOAT,
                InputSlot: 0,
                AlignedByteOffset: 0,
                InputSlotClass: D3D11_INPUT_PER_VERTEX_DATA,
                InstanceDataStepRate: 0,
            },
            D3D11_INPUT_ELEMENT_DESC {
                SemanticName: s!("TEX"),
                SemanticIndex: 0,
                Format: DXGI_FORMAT_R32G32_FLOAT,
                InputSlot: 0,
                AlignedByteOffset: 12,
                InputSlotClass: D3D11_INPUT_PER_VERTEX_DATA,
                InstanceDataStepRate: 0,
            },
        ];

        let input_layout = unsafe {
            let mut input_layout = None;
            device.CreateInputLayout(&input_desc, vertex_shader_bytes, Some(&mut input_layout))?;
            input_layout.unwrap()
        };

        let vertex_buffer = unsafe {
            let desc = D3D11_BUFFER_DESC {
                ByteWidth: mem::size_of_val(&VERTEX_DATA) as u32,
                Usage: D3D11_USAGE_IMMUTABLE,
                BindFlags: D3D11_BIND_VERTEX_BUFFER.0 as u32,
                ..Default::default()
            };

            let sr_data = D3D11_SUBRESOURCE_DATA {
                pSysMem: VERTEX_DATA.as_ptr().cast(),
                ..Default::default()
            };

            let mut vertex_buffer = None;

            device.CreateBuffer(&desc, Some(&sr_data), Some(&mut vertex_buffer))?;

            vertex_buffer.unwrap()
        };

        let sampler_state = unsafe {
            let desc = D3D11_SAMPLER_DESC {
                Filter: D3D11_FILTER_MIN_MAG_MIP_POINT,
                AddressU: D3D11_TEXTURE_ADDRESS_CLAMP,
                AddressV: D3D11_TEXTURE_ADDRESS_CLAMP,
                AddressW: D3D11_TEXTURE_ADDRESS_CLAMP,
                ComparisonFunc: D3D11_COMPARISON_NEVER,
                MinLOD: 0.0,
                MaxLOD: D3D11_FLOAT32_MAX,
                ..Default::default()
            };

            let mut sampler_state = None;

            device.CreateSamplerState(&desc, Some(&mut sampler_state))?;

            sampler_state.unwrap()
        };

        let context = unsafe { device.GetImmediateContext()? };

        Ok(Renderer {
            device,
            context,
            vertex_shader,
            pixel_shader,
            vertex_buffer,
            input_layout,
            sampler_state,
        })
    }

    pub fn create_render_texture(
        &self,
        width: u32,
        height: u32,
    ) -> eyre::Result<(ID3D11Texture2D, ID3D11ShaderResourceView)> {
        let render_texture = unsafe {
            let mut texture = None;
            let texture_desc = D3D11_TEXTURE2D_DESC {
                Width: width,
                Height: height,
                Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                MipLevels: 1,
                ArraySize: 1,
                SampleDesc: DXGI_SAMPLE_DESC {
                    Count: 1,
                    Quality: 0,
                },
                Usage: D3D11_USAGE_DEFAULT,
                BindFlags: (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32,
                CPUAccessFlags: D3D11_CPU_ACCESS_FLAG::default().0 as u32,
                MiscFlags: D3D11_RESOURCE_MISC_SHARED.0 as u32,
            };

            self.device
                .CreateTexture2D(&texture_desc, None, Some(&mut texture))?;

            texture.unwrap()
        };

        let resource_view = unsafe {
            let mut resource_view = None;

            self.device.CreateShaderResourceView(
                &render_texture,
                None,
                Some(&mut resource_view),
            )?;

            resource_view.unwrap()
        };

        Ok((render_texture, resource_view))
    }

    pub fn draw_flipped_texture(
        &self,
        src_texture: &ID3D11ShaderResourceView,
        target: &ID3D11Texture2D,
        size: (u32, u32),
        offset: (i32, i32),
    ) -> eyre::Result<()> {
        unsafe {
            let mut target_desc = Default::default();
            target.GetDesc(&mut target_desc);

            let rtv = {
                let mut rtv = None;
                self.device
                    .CreateRenderTargetView(target, None, Some(&mut rtv))?;
                rtv.unwrap()
            };

            self.context
                .ClearRenderTargetView(&rtv, &[0.0, 0.0, 0.0, 0.0]);

            self.context.RSSetViewports(Some(&[D3D11_VIEWPORT {
                TopLeftX: offset.0 as f32,
                TopLeftY: offset.1 as f32,
                Width: size.0 as f32,
                Height: size.1 as f32,
                MinDepth: 0.0,
                MaxDepth: 1.0,
            }]));

            self.context.OMSetRenderTargets(Some(&[Some(rtv)]), None);
            self.context
                .IASetPrimitiveTopology(D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
            self.context.IASetInputLayout(&self.input_layout);
            self.context.IASetVertexBuffers(
                0,
                1,
                Some(&Some(self.vertex_buffer.clone())),
                Some(&(mem::size_of::<Vertex>() as u32)),
                Some(&0),
            );

            self.context.VSSetShader(&self.vertex_shader, None);
            self.context.PSSetShader(&self.pixel_shader, None);

            self.context
                .PSSetShaderResources(0, Some(&[Some(src_texture.clone())]));

            self.context
                .PSSetSamplers(0, Some(&[Some(self.sampler_state.clone())]));

            self.context.Draw(VERTEX_DATA.len() as u32, 0);
        }

        Ok(())
    }
}
