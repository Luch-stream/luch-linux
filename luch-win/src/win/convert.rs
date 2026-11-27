//! BGRA to NV12 (BT.709, limited range) and scaling on the GPU with the D3D11 video processor, the fixed-function
//! block next to the encoder. Sunshine draws the same with two pixel shaders (display_vram.cpp:513-556); the video
//! processor needs no shader code and every vendor has it. Same job as luch-kms's two GLES passes.

use std::collections::HashMap;
use std::mem::ManuallyDrop;

use windows::core::Interface;
use windows::Win32::Foundation::RECT;
use windows::Win32::Graphics::Direct3D11::{
    ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D, ID3D11VideoContext, ID3D11VideoContext1,
    ID3D11VideoDevice, ID3D11VideoProcessor, ID3D11VideoProcessorEnumerator,
    ID3D11VideoProcessorInputView, ID3D11VideoProcessorOutputView, D3D11_BIND_RENDER_TARGET,
    D3D11_TEX2D_VPIV, D3D11_TEX2D_VPOV, D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT,
    D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE, D3D11_VIDEO_PROCESSOR_CONTENT_DESC,
    D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC, D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0,
    D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC, D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0,
    D3D11_VIDEO_PROCESSOR_STREAM, D3D11_VIDEO_USAGE_OPTIMAL_SPEED, D3D11_VPIV_DIMENSION_TEXTURE2D,
    D3D11_VPOV_DIMENSION_TEXTURE2D,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P709, DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_LEFT_P709,
    DXGI_FORMAT_NV12, DXGI_RATIONAL, DXGI_SAMPLE_DESC,
};

pub struct Converter {
    vdev: ID3D11VideoDevice,
    vctx: ID3D11VideoContext,
    venum: ID3D11VideoProcessorEnumerator,
    vp: ID3D11VideoProcessor,
    /// The encoder's input picture, NV12 at the session size.
    pub nv12: ID3D11Texture2D,
    out_view: ID3D11VideoProcessorOutputView,
    /// One input view per pool texture, made once.
    in_views: HashMap<usize, ID3D11VideoProcessorInputView>,
}

impl Converter {
    pub fn new(
        dev: &ID3D11Device,
        ctx: &ID3D11DeviceContext,
        input: (u32, u32),
        output: (u32, u32),
        fps: u32,
    ) -> windows::core::Result<Self> {
        let rate = DXGI_RATIONAL {
            Numerator: fps,
            Denominator: 1,
        };
        let content = D3D11_VIDEO_PROCESSOR_CONTENT_DESC {
            InputFrameFormat: D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
            InputFrameRate: rate,
            InputWidth: input.0,
            InputHeight: input.1,
            OutputFrameRate: rate,
            OutputWidth: output.0,
            OutputHeight: output.1,
            Usage: D3D11_VIDEO_USAGE_OPTIMAL_SPEED,
        };
        // SAFETY: COM calls on valid interfaces with valid descriptors and out-pointers.
        unsafe {
            let vdev: ID3D11VideoDevice = dev.cast()?;
            let vctx: ID3D11VideoContext = ctx.cast()?;
            let venum = vdev.CreateVideoProcessorEnumerator(&content)?;
            let vp = vdev.CreateVideoProcessor(&venum, 0)?;
            if let Ok(v1) = vctx.cast::<ID3D11VideoContext1>() {
                v1.VideoProcessorSetStreamColorSpace1(
                    &vp,
                    0,
                    DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P709,
                );
                v1.VideoProcessorSetOutputColorSpace1(
                    &vp,
                    DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_LEFT_P709,
                );
            }
            // Letterbox nothing and keep the whole picture: source and destination are the full frames.
            let full = |(w, h): (u32, u32)| RECT {
                left: 0,
                top: 0,
                right: w as i32,
                bottom: h as i32,
            };
            vctx.VideoProcessorSetStreamSourceRect(&vp, 0, true, Some(&full(input)));
            vctx.VideoProcessorSetStreamDestRect(&vp, 0, true, Some(&full(output)));
            vctx.VideoProcessorSetOutputTargetRect(&vp, true, Some(&full(output)));
            vctx.VideoProcessorSetStreamAutoProcessingMode(&vp, 0, false);

            let desc = D3D11_TEXTURE2D_DESC {
                Width: output.0,
                Height: output.1,
                MipLevels: 1,
                ArraySize: 1,
                Format: DXGI_FORMAT_NV12,
                SampleDesc: DXGI_SAMPLE_DESC {
                    Count: 1,
                    Quality: 0,
                },
                Usage: D3D11_USAGE_DEFAULT,
                BindFlags: D3D11_BIND_RENDER_TARGET.0 as u32,
                CPUAccessFlags: 0,
                MiscFlags: 0,
            };
            let mut nv12 = None;
            dev.CreateTexture2D(&desc, None, Some(&mut nv12))?;
            let nv12: ID3D11Texture2D = nv12.unwrap();
            let ov = D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC {
                ViewDimension: D3D11_VPOV_DIMENSION_TEXTURE2D,
                Anonymous: D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0 {
                    Texture2D: D3D11_TEX2D_VPOV { MipSlice: 0 },
                },
            };
            let mut out_view = None;
            vdev.CreateVideoProcessorOutputView(&nv12, &venum, &ov, Some(&mut out_view))?;
            Ok(Converter {
                vdev,
                vctx,
                venum,
                vp,
                nv12,
                out_view: out_view.unwrap(),
                in_views: HashMap::new(),
            })
        }
    }

    /// Draws `src` into `self.nv12`. The GPU runs it in order before the encoder reads the texture; no wait here.
    pub fn convert(&mut self, src: &ID3D11Texture2D) -> windows::core::Result<()> {
        let key = src.as_raw() as usize;
        if !self.in_views.contains_key(&key) {
            let iv = D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC {
                FourCC: 0,
                ViewDimension: D3D11_VPIV_DIMENSION_TEXTURE2D,
                Anonymous: D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0 {
                    Texture2D: D3D11_TEX2D_VPIV {
                        MipSlice: 0,
                        ArraySlice: 0,
                    },
                },
            };
            let mut view = None;
            // SAFETY: valid interfaces, descriptor and out-pointer.
            unsafe {
                self.vdev
                    .CreateVideoProcessorInputView(src, &self.venum, &iv, Some(&mut view))?
            };
            self.in_views.insert(key, view.unwrap());
        }
        let stream = D3D11_VIDEO_PROCESSOR_STREAM {
            Enable: true.into(),
            pInputSurface: ManuallyDrop::new(Some(self.in_views[&key].clone())),
            ..Default::default()
        };
        let streams = [stream];
        // SAFETY: valid interfaces; the stream's view reference is dropped by hand below.
        let r = unsafe {
            self.vctx
                .VideoProcessorBlt(&self.vp, &self.out_view, 0, &streams)
        };
        let [mut s] = streams;
        // SAFETY: taken once, the struct is not used again.
        unsafe { ManuallyDrop::drop(&mut s.pInputSurface) };
        r
    }
}
