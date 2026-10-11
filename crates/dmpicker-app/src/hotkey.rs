//! 검색창 단축키: 받을 수 있는 조합인지, 보이는 이름 ("Ctrl + Space"), 설정 파일에 쓰는 글

use windows::Win32::UI::Input::KeyboardAndMouse::*;

/// 검색창 단축키: 수식 키 (`MOD_CONTROL`·`MOD_ALT`·`MOD_SHIFT`)와 수식 키가 아닌 키 하나
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Hotkey {
    pub mods: HOT_KEY_MODIFIERS,
    pub vk: VIRTUAL_KEY,
}

const fn hk(mods: HOT_KEY_MODIFIERS, vk: VIRTUAL_KEY) -> Hotkey {
    Hotkey { mods, vk }
}

/// Windows가 쓰는 조합: 창 메뉴, 창 닫기
const SYSTEM: [Hotkey; 2] = [hk(MOD_ALT, VK_SPACE), hk(MOD_ALT, VK_F4)];

const CTRL_SHIFT: HOT_KEY_MODIFIERS = HOT_KEY_MODIFIERS(MOD_CONTROL.0 | MOD_SHIFT.0);

/// 검색창 안에서 쓰는 조합 (popup.rs의 키 처리), 단축키가 되면 검색창을 닫아 버림
/// 모드·난이도 바꾸기, 입력 칸 편집 (전체 선택, 복사·잘라내기·붙여넣기, 단어 이동과 선택)
const IN_POPUP: [Hotkey; 12] = [
    hk(MOD_ALT, VIRTUAL_KEY(b'M' as u16)),
    hk(HOT_KEY_MODIFIERS(MOD_ALT.0 | MOD_SHIFT.0), VIRTUAL_KEY(b'M' as u16)),
    hk(MOD_ALT, VK_LEFT),
    hk(MOD_ALT, VK_RIGHT),
    hk(MOD_CONTROL, VIRTUAL_KEY(b'A' as u16)),
    hk(MOD_CONTROL, VIRTUAL_KEY(b'C' as u16)),
    hk(MOD_CONTROL, VIRTUAL_KEY(b'X' as u16)),
    hk(MOD_CONTROL, VIRTUAL_KEY(b'V' as u16)),
    hk(MOD_CONTROL, VK_LEFT),
    hk(MOD_CONTROL, VK_RIGHT),
    hk(CTRL_SHIFT, VK_LEFT),
    hk(CTRL_SHIFT, VK_RIGHT),
];

/// 표시 순서대로 수식 키와 그 이름
const MOD_NAMES: [(HOT_KEY_MODIFIERS, &str); 3] = [(MOD_CONTROL, "Ctrl"), (MOD_ALT, "Alt"), (MOD_SHIFT, "Shift")];

/// 받는 키의 이름: 문자, 숫자, F1~F24, Space, 화살표, 기호 (미국 배열 글자)
fn key_name(vk: VIRTUAL_KEY) -> Option<String> {
    let v = vk.0;
    let name = match vk {
        _ if (0x41..=0x5A).contains(&v) || (0x30..=0x39).contains(&v) => return Some(char::from(v as u8).to_string()),
        _ if (VK_F1.0..=VK_F24.0).contains(&v) => return Some(format!("F{}", v - VK_F1.0 + 1)),
        VK_SPACE => "Space",
        VK_LEFT => "Left",
        VK_UP => "Up",
        VK_RIGHT => "Right",
        VK_DOWN => "Down",
        VK_OEM_1 => ";",
        VK_OEM_PLUS => "=",
        VK_OEM_COMMA => ",",
        VK_OEM_MINUS => "-",
        VK_OEM_PERIOD => ".",
        VK_OEM_2 => "/",
        VK_OEM_3 => "`",
        VK_OEM_4 => "[",
        VK_OEM_5 => "\\",
        VK_OEM_6 => "]",
        VK_OEM_7 => "'",
        _ => return None,
    };
    Some(name.into())
}

impl Hotkey {
    pub const DEFAULT: Hotkey = hk(MOD_CONTROL, VK_SPACE);

    /// 누른 조합을 단축키로 받을지, 못 받으면 검색창에 보일 이유
    /// `mods`에 `MOD_WIN`이 있을 수 있음 (Win 조합은 거부)
    pub fn check(self) -> Result<Hotkey, &'static str> {
        if self.mods.0 & MOD_WIN.0 != 0 || SYSTEM.contains(&self) {
            return Err("Windows에서 쓰는 키예요");
        }
        if self.mods.0 & (MOD_CONTROL.0 | MOD_ALT.0) == 0 {
            return Err("Ctrl이나 Alt를 함께 눌러 주세요");
        }
        if key_name(self.vk).is_none() {
            return Err("이 키는 쓸 수 없어요");
        }
        if IN_POPUP.contains(&self) {
            return Err("검색창에서 쓰는 키예요");
        }
        Ok(self)
    }

    /// "Ctrl + Shift + K"
    pub fn label(self) -> String {
        let mods = MOD_NAMES.iter().filter(|(m, _)| self.mods.0 & m.0 != 0).map(|&(_, n)| n.to_string());
        mods.chain(key_name(self.vk)).collect::<Vec<_>>().join(" + ")
    }

    /// [`Hotkey::label`]로 쓴 글을 다시 읽음, 받을 수 없는 조합이면 None
    pub fn parse(s: &str) -> Option<Hotkey> {
        let mut parts: Vec<&str> = s.split('+').map(str::trim).collect();
        let key = parts.pop()?;
        let mut mods = 0;
        for p in parts {
            mods |= MOD_NAMES.iter().find(|(_, n)| *n == p)?.0 .0;
        }
        let vk = (0..=0xFF).map(VIRTUAL_KEY).find(|&v| key_name(v).as_deref() == Some(key))?;
        hk(HOT_KEY_MODIFIERS(mods), vk).check().ok()
    }
}
