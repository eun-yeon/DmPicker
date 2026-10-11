//! 작업이 게임을 조작하는 동안 플레이어가 직접 누른 키·클릭·휠을 raw input으로 감지
//! 작업은 SendInput으로 키만 눌러 장치가 없고 마우스는 안 씀, 마우스 버튼은 전부 플레이어 것
//! (정밀 터치패드도 장치 없이 올 수 있음), 움직이기만 하는 마우스는 게임이 안 따라가서 무시함
//! 지켜보기만 하고 훅이나 입력 주입은 없음

use std::cell::Cell;

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, HOT_KEY_MODIFIERS, MOD_ALT, MOD_CONTROL, MOD_SHIFT, VIRTUAL_KEY, VK_CONTROL, VK_LCONTROL, VK_LMENU, VK_LSHIFT, VK_MENU, VK_RCONTROL,
    VK_RMENU, VK_RSHIFT, VK_SHIFT,
};
use windows::Win32::UI::Input::*;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::hotkey::Hotkey;

const MOUSE: u16 = 2;
const KEYBOARD: u16 = 6;
const KEY_UP: u16 = 1;
const KEY_E0: u16 = 2;
/// 오른쪽 Shift의 스캔 코드 (Ctrl·Alt는 E0 표시로 가림)
const RIGHT_SHIFT: u16 = 0x36;

/// 단축키 수식 키와 raw input이 주는 키, 왼쪽·오른쪽 키 (`StopKey::held` 순서)
const MOD_KEYS: [(HOT_KEY_MODIFIERS, VIRTUAL_KEY, [VIRTUAL_KEY; 2]); 3] = [
    (MOD_CONTROL, VK_CONTROL, [VK_LCONTROL, VK_RCONTROL]),
    (MOD_ALT, VK_MENU, [VK_LMENU, VK_RMENU]),
    (MOD_SHIFT, VK_SHIFT, [VK_LSHIFT, VK_RSHIFT]),
];

/// 만든 스레드에서 drop될 때까지 감지, 등록이 프로세스 단위라 한 번에 하나만
pub struct Hands {
    hwnd: HWND,
    keys: StopKey,
    touched: Cell<bool>,
}

/// 키 입력에서 작업을 멈추는 검색창 단축키만 빼고 나머지는 손댄 걸로 침
/// 단축키의 수식 키는 누를 땐 기다리고, 다 뗄 때까지 단축키를 안 눌렀으면 손댄 것 (Shift만 눌러도 탭이 바뀜)
struct StopKey {
    stop: Hotkey,
    /// Ctrl·Alt·Shift의 왼쪽·오른쪽 눌림
    held: Cell<[[bool; 2]; 3]>,
    /// 단축키 수식 키를 누른 뒤로 단축키를 눌렀음, 그 수식 키를 다 떼면 다시 false
    used: Cell<bool>,
}

fn register(flags: RAWINPUTDEVICE_FLAGS, target: HWND) -> bool {
    let devices = [MOUSE, KEYBOARD].map(|usage| RAWINPUTDEVICE { usUsagePage: 1, usUsage: usage, dwFlags: flags, hwndTarget: target });
    unsafe { RegisterRawInputDevices(&devices, size_of::<RAWINPUTDEVICE>() as u32) }.is_ok()
}

/// Ctrl·Alt·Shift가 `held`처럼 눌린 채 누른 `vk`가 멈추는 단축키 그대로인지 (수식 키가 딱 맞아야 함)
fn is_stop(stop: Hotkey, held: [[bool; 2]; 3], vk: u16) -> bool {
    let mods = MOD_KEYS.iter().zip(held).filter(|(_, sides)| sides.contains(&true)).fold(0, |a, ((m, ..), _)| a | m.0);
    vk == stop.vk.0 && mods == stop.mods.0
}

impl StopKey {
    /// `held`는 지금 눌린 수식 키, 이미 눌린 단축키 수식 키는 단축키를 누르던 걸로 봄
    fn new(stop: Hotkey, held: [[bool; 2]; 3]) -> Self {
        let k = StopKey { stop, held: Cell::new(held), used: Cell::new(false) };
        k.used.set(k.stop_mods_held());
        k
    }

    fn stop_mods_held(&self) -> bool {
        MOD_KEYS.iter().zip(self.held.get()).any(|((m, ..), sides)| self.stop.mods.0 & m.0 != 0 && sides.contains(&true))
    }

    fn is_stop_mod(&self, m: usize) -> bool {
        self.stop.mods.0 & MOD_KEYS[m].0 .0 != 0
    }

    /// 키 `vk`를 누르거나 뗌 (수식 키면 `right`가 오른쪽 키), 손댄 거면 true
    fn key(&self, vk: u16, right: bool, up: bool) -> bool {
        if let Some(m) = MOD_KEYS.iter().position(|(_, k, _)| k.0 == vk) {
            let side = usize::from(right);
            if up {
                return self.release(m, side);
            }
            let mut held = self.held.get();
            held[m][side] = true;
            self.held.set(held);
            return !self.is_stop_mod(m);
        }
        if up {
            return false;
        }
        if is_stop(self.stop, self.held.get(), vk) {
            self.used.set(true);
            return false;
        }
        true
    }

    /// 수식 키 `m`의 한쪽을 뗌, 손댄 거면 true (단축키 수식 키를 단축키 없이 다 뗌)
    /// 이미 뗀 키는 무시함 (`forget_released`가 먼저 본 키의 뒤늦은 key-up)
    fn release(&self, m: usize, side: usize) -> bool {
        let mut held = self.held.get();
        if !held[m][side] {
            return false;
        }
        held[m][side] = false;
        self.held.set(held);
        if !self.is_stop_mod(m) {
            return false; // 누를 때 셈
        }
        let used = self.used.get();
        if !self.stop_mods_held() {
            self.used.set(false);
        }
        !used
    }

    /// key-up을 놓쳐도 (키보드 뽑힘, 다른 데스크톱) 지금 안 눌린 수식 키는 뗀 것으로 판정, 손댄 게 있으면 true
    fn forget_released(&self, now: [[bool; 2]; 3]) -> bool {
        let mut touched = false;
        for (m, sides) in now.iter().enumerate() {
            for (side, &down) in sides.iter().enumerate() {
                if !down {
                    touched |= self.release(m, side);
                }
            }
        }
        touched
    }
}

/// 지금 눌려 있는 Ctrl·Alt·Shift의 왼쪽·오른쪽
fn held_now() -> [[bool; 2]; 3] {
    MOD_KEYS.map(|(_, _, sides)| sides.map(|k| unsafe { GetAsyncKeyState(k.0 as i32) } < 0))
}

impl Hands {
    /// `stop`은 검색창 단축키, raw input을 못 쓰면 None
    pub fn watch(stop: Hotkey) -> Option<Hands> {
        let hwnd = unsafe { CreateWindowExW(WINDOW_EX_STYLE(0), w!("STATIC"), PCWSTR::null(), WINDOW_STYLE(0), 0, 0, 0, 0, Some(HWND_MESSAGE), None, None, None) }.ok()?;
        if !register(RIDEV_INPUTSINK, hwnd) {
            unsafe {
                let _ = DestroyWindow(hwnd);
            }
            return None;
        }
        // 수식 키가 이미 눌려 있을 수 있음 (단축키 누르는 중)
        Some(Hands { hwnd, keys: StopKey::new(stop, held_now()), touched: Cell::new(false) })
    }

    /// 감지 시작 뒤로 플레이어가 키를 누르거나 클릭·스크롤했는지
    /// 중지 키인 단축키는 안 셈
    pub fn touched(&self) -> bool {
        let mut msg = MSG::default();
        while unsafe { PeekMessageW(&mut msg, Some(self.hwnd), WM_INPUT, WM_INPUT, PM_REMOVE) }.as_bool() {
            if let Some(what) = self.player(msg.lParam) {
                if !self.touched.replace(true) {
                    eprintln!("hands: {what}");
                }
            }
            unsafe { DispatchMessageW(&msg) };
        }
        if self.keys.forget_released(held_now()) && !self.touched.replace(true) {
            eprintln!("hands: modifier released without the hotkey (key-up missed)");
        }
        self.touched.get()
    }

    /// 이 메시지에 담긴 플레이어 입력 설명 (로그용)
    fn player(&self, l: LPARAM) -> Option<String> {
        let mut raw = RAWINPUT::default();
        let mut size = size_of::<RAWINPUT>() as u32;
        let n = unsafe { GetRawInputData(HRAWINPUT(l.0 as _), RID_INPUT, Some(&mut raw as *mut _ as _), &mut size, size_of::<RAWINPUTHEADER>() as u32) };
        if n == u32::MAX {
            return None;
        }
        if raw.header.dwType == RIM_TYPEKEYBOARD.0 {
            let k = unsafe { raw.data.keyboard };
            if raw.header.hDevice.0.is_null() || k.VKey == 0xFF {
                return None; // 우리가 누른 키, 일부 키보드가 보내는 빈 값
            }
            let up = k.Flags & KEY_UP != 0;
            let right = if k.VKey == VK_SHIFT.0 { k.MakeCode == RIGHT_SHIFT } else { k.Flags & KEY_E0 != 0 };
            self.keys.key(k.VKey, right, up).then(|| format!("key {:#04x}{}", k.VKey, if up { " up" } else { "" }))
        } else if raw.header.dwType == RIM_TYPEMOUSE.0 {
            // 버튼과 휠만, 움직이기만 하면 플래그 없음
            let m = unsafe { raw.data.mouse };
            let buttons = unsafe { m.Anonymous.Anonymous.usButtonFlags };
            let data = unsafe { m.Anonymous.Anonymous.usButtonData } as i16;
            let device = if raw.header.hDevice.0.is_null() { "none" } else { "yes" };
            (buttons != 0).then(|| format!("mouse buttons {buttons:#06x} data {data} flags {:#06x} move {},{} device {device}", m.usFlags.0, m.lLastX, m.lLastY))
        } else {
            None
        }
    }
}

impl Drop for Hands {
    fn drop(&mut self) {
        register(RIDEV_REMOVE, HWND::default());
        unsafe {
            let _ = DestroyWindow(self.hwnd);
        }
    }
}
