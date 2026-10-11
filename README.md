<div align="center">
  <img src="crates/dmpicker-app/assets/icon.ico" alt="DM PICKER" width="120" height="120">

  <h1>DM PICKER</h1>

  <p>
    <strong>찾는 곡으로 바로 이동하는 곡 검색기</strong>
  </p>
</div>

<div align="center">
  <img src=".github/assets/preview.png" alt="Screenshot" width="560">
</div>

## 개요

**DM PICKER**는 DJMAX RESPECT V 선곡 화면에서 쓰는 곡 검색기입니다.

단축키로 검색창을 열고 곡 이름을 입력한 뒤 Enter를 누르면 목록을 넘기지 않아도 곡을 찾아줍니다.

**지원 환경** · Windows 10/11

[다운로드](https://github.com/eun-yeon/DmPicker/releases/latest)

## 주요 기능

**검색**: 곡 이름 · 초성 · 작곡가 · 별명 · 레벨 (`sc15`, `4b sc14-15`) · BPM (`bpm:150~180`, `sc14 bpm:200~`)

**이동**: 고른 곡으로 바로 이동 · 버튼 모드와 난이도 맞추기 · 무작위로 고르기 · 최근 고른 곡

**화면**: 프리스타일과 온라인 방 · 해상도와 화면 비율 자동 인식

## 설치

위 링크에서 최신 버전을 받아 압축을 풀고 실행하면 됩니다. 실행 파일 옆의 `data` 폴더는 함께 두어 주세요.

실행하면 작업 표시줄 알림 영역에 아이콘이 생깁니다. 아이콘 메뉴에서 Windows 시작 시 실행을 켤 수 있습니다.

설정과 인식한 곡 목록은 `%appdata%/DmPicker` 폴더에 저장됩니다.

## 사용법

1. 게임 선곡 화면에서 `Ctrl + Space`를 눌러 검색창을 엽니다.
2. 처음 한 번은 **목록 인식**으로 내 곡 목록을 읽어 와야 합니다. 읽는 동안에는 키보드와 마우스를 잠시 쉬어 주세요.
3. 곡을 검색하고 Enter를 누르면 그 곡으로 이동합니다.

버튼 모드, 난이도, 검색창 단축키는 검색창 오른쪽의 설정에서 바꿀 수 있습니다.

## 사용 팁

> [!TIP]
> DLC를 사거나 곡을 해금했을 때, 다른 계정으로 바꿨을 때는 설정에서 **다시 인식**해 주세요.

정렬이 곡 제목순이 아니면 곡 제목 (A to Z)으로 바꾼 뒤 이동합니다.

이동하는 동안 다른 창으로 바꾸면 바로 멈춥니다.

단축키가 다른 프로그램과 겹쳐 검색창이 열리지 않으면 알림 영역 아이콘 메뉴에서 `Ctrl + Space`로 되돌릴 수 있습니다.

> [!NOTE]
> DM PICKER는 화면을 보고 키보드 입력만 보냅니다. 게임 파일이나 메모리는 건드리지 않습니다.

## 개발

### 기술 스택

- **언어**: Rust
- **화면**: Direct2D · DirectComposition
- **캡처**: PrintWindow · GDI

### 빌드

```bash
git clone https://github.com/eun-yeon/DmPicker.git
cd DmPicker
cargo build --release
```

`target/release/DmPicker.exe`가 만들어집니다. 저장소의 `data` 폴더를 자동으로 찾기 때문에 따로 복사하지 않아도 바로 실행할 수 있습니다.

## 라이선스

[GPL-3.0 License](LICENSE)

DM PICKER는 NEOWIZ와 관련 없는 비공식 유틸입니다. 자세한 내용은 [NOTICE](licenses/NOTICE.txt)를 확인해 주세요.
