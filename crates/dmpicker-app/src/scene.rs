//! 창을 윈도우 합성기 (Windows.UI.Composition)에 올림
//! 그린 화면 (swapchain) 한 장 아래위로 빛 비주얼을 겹침, 빛의 움직임은 합성기가 화면 주사율로 그림
//! 합성기를 못 만들면 `Gfx`가 DirectComposition으로 그린 화면만 올림 (빛 없음)

use std::cell::OnceCell;

use windows::core::{s, w, Interface, Result, HRESULT};
use windows::System::DispatcherQueueController;
use windows::UI::Composition::Desktop::DesktopWindowTarget;
use windows::UI::Composition::{CompositionGraphicsDevice, Compositor, ContainerVisual};
use windows::Win32::Foundation::{E_FAIL, HWND};
use windows::Win32::Graphics::Direct2D::ID2D1Device;
use windows::Win32::Graphics::Dxgi::IDXGISwapChain1;
use windows::Win32::System::WinRT::Composition::{ICompositorDesktopInterop, ICompositorInterop};
use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryExW, LOAD_LIBRARY_SEARCH_SYSTEM32};
use windows::Win32::System::WinRT::{DispatcherQueueOptions, DQTAT_COM_NONE, DQTYPE_THREAD_CURRENT};
use windows_numerics::{Matrix3x2, Matrix4x4, Vector2};

thread_local! {
    // UI 스레드에 하나, 검색창과 알림이 같이 씀 (못 만들면 None)
    static COMPOSITOR: OnceCell<Option<(DispatcherQueueController, Compositor)>> = const { OnceCell::new() };
}

/// UI 스레드의 합성기, 처음 부를 때 만듦 (합성기 일은 이 스레드의 메시지 루프가 돌림)
fn compositor() -> Option<Compositor> {
    COMPOSITOR.with(|c| {
        c.get_or_init(|| {
            let make = || -> Result<(DispatcherQueueController, Compositor)> { Ok((dispatcher_queue()?, Compositor::new()?)) };
            make().inspect_err(|e| eprintln!("compositor: {e:?}")).ok()
        })
        .as_ref()
        .map(|(_, c)| c.clone())
    })
}

/// 이 스레드의 디스패처 큐, 만드는 함수는 실행 중에 찾음
/// 1709 이전 윈도우엔 없는 함수라 바로 링크하면 앱이 안 켜짐
fn dispatcher_queue() -> Result<DispatcherQueueController> {
    type Create = unsafe extern "system" fn(DispatcherQueueOptions, *mut *mut std::ffi::c_void) -> HRESULT;
    unsafe {
        let lib = LoadLibraryExW(w!("CoreMessaging.dll"), None, LOAD_LIBRARY_SEARCH_SYSTEM32)?;
        let found = GetProcAddress(lib, s!("CreateDispatcherQueueController")).ok_or_else(|| windows::core::Error::from(E_FAIL))?;
        let create: Create = std::mem::transmute(found);
        let options = DispatcherQueueOptions {
            dwSize: std::mem::size_of::<DispatcherQueueOptions>() as u32,
            threadType: DQTYPE_THREAD_CURRENT,
            apartmentType: DQTAT_COM_NONE,
        };
        let mut queue = std::ptr::null_mut();
        create(options, &mut queue).ok()?;
        Ok(DispatcherQueueController::from_raw(queue))
    }
}

pub struct Scene {
    pub compositor: Compositor,
    target: DesktopWindowTarget,
    _root: ContainerVisual,
    /// 그린 화면 아래 (판 밖으로 번지는 빛)와 위 (테두리 빛), 둘 다 창 좌표 (px)
    pub under: ContainerVisual,
    pub over: ContainerVisual,
    /// 한 번 그려 두는 빛 모양을 Direct2D로 그릴 장치
    pub graphics: CompositionGraphicsDevice,
}

impl Scene {
    /// 그린 화면을 가운데 층으로 쌓고 맨 마지막에 창에 붙임
    /// 중간에 실패하면 창은 그대로라 DirectComposition으로 올릴 수 있음
    pub fn new(hwnd: HWND, swapchain: &IDXGISwapChain1, device: &ID2D1Device, w: f32, h: f32) -> Result<Self> {
        let compositor = compositor().ok_or_else(|| windows::core::Error::from(E_FAIL))?;
        let size = Vector2 { X: w, Y: h };
        unsafe {
            let root = compositor.CreateContainerVisual()?;
            root.SetSize(size)?;
            let interop: ICompositorInterop = compositor.cast()?;
            let content = compositor.CreateSpriteVisual()?;
            content.SetSize(size)?;
            content.SetBrush(&compositor.CreateSurfaceBrushWithSurface(&interop.CreateCompositionSurfaceForSwapChain(swapchain)?)?)?;
            let under = compositor.CreateContainerVisual()?;
            let over = compositor.CreateContainerVisual()?;
            for v in [&under, &over] {
                v.SetSize(size)?;
            }
            let layers = root.Children()?;
            layers.InsertAtTop(&under)?;
            layers.InsertAtTop(&content)?;
            layers.InsertAtTop(&over)?;
            let graphics = interop.CreateGraphicsDevice(device)?;
            let target = compositor.cast::<ICompositorDesktopInterop>()?.CreateDesktopWindowTarget(hwnd, true)?;
            target.SetRoot(&root)?;
            Ok(Self { compositor, target, _root: root, under, over, graphics })
        }
    }

    /// 빛 층 전체에 변환 (검색창이 닫히며 한 덩어리로 줄어들 때)
    pub fn set_transform(&self, xf: &Matrix3x2) -> Result<()> {
        let m = matrix4(xf);
        self.under.SetTransformMatrix(m)?;
        self.over.SetTransformMatrix(m)
    }
}

impl Drop for Scene {
    /// 창에서 바로 떼어 냄, 같은 창에 새로 붙일 수 있게 (장치를 다시 만들 때)
    fn drop(&mut self) {
        let _ = self.target.Close();
    }
}

/// 평면 변환을 비주얼 변환으로
pub fn matrix4(xf: &Matrix3x2) -> Matrix4x4 {
    Matrix4x4 {
        M11: xf.M11,
        M12: xf.M12,
        M13: 0.0,
        M14: 0.0,
        M21: xf.M21,
        M22: xf.M22,
        M23: 0.0,
        M24: 0.0,
        M31: 0.0,
        M32: 0.0,
        M33: 1.0,
        M34: 0.0,
        M41: xf.M31,
        M42: xf.M32,
        M43: 0.0,
        M44: 1.0,
    }
}
