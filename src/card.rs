// Cartão flutuante da seleção de campeão: counters antes de escolher, builds depois.
// Janela Win32 desenhada com GDI (sem navegador embutido), numa thread própria.
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use std::sync::{Mutex, OnceLock};
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM};
use windows_sys::Win32::Graphics::Dwm::DwmSetWindowAttribute;
use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::HiDpi::GetDpiForWindow;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT};
use windows_sys::Win32::UI::Shell::ShellExecuteW;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

#[derive(Clone)]
pub enum Action {
    /// Marca o campeão na seleção (sem confirmar)
    Hover(i64),
    /// Troca para a página de runas de índice N
    Runes(usize),
}

#[derive(Clone, Default)]
pub struct Row {
    pub text: String,
    pub detail: String,
    /// Some(true/false) desenha ● / ○ à esquerda
    pub marker: Option<bool>,
    /// Texto longo, quebra em várias linhas
    pub wrap: bool,
    pub action: Option<Action>,
}

#[derive(Clone, Default)]
pub struct View {
    pub title: String,
    pub subtitle: String,
    pub sections: Vec<(String, Vec<Row>)>,
    pub link: Option<String>,
}

#[derive(Clone)]
enum Hit {
    Close,
    Link(String),
    Act(Action),
}

static VIEW: Mutex<Option<View>> = Mutex::new(None);
static HITS: Mutex<Vec<(RECT, Hit)>> = Mutex::new(Vec::new());
static HOVER: Mutex<Option<usize>> = Mutex::new(None);
static HANDLE: AtomicIsize = AtomicIsize::new(0);
static MOVED: AtomicBool = AtomicBool::new(false);
static HANDLER: OnceLock<Box<dyn Fn(Action) + Send + Sync>> = OnceLock::new();

const WM_UPDATE: u32 = WM_APP + 1;
const WM_MOUSELEAVE: u32 = 0x02A3;
const WIDTH: i32 = 330;

const fn rgb(r: u8, g: u8, b: u8) -> u32 {
    r as u32 | (g as u32) << 8 | (b as u32) << 16
}
const BG: u32 = rgb(0x12, 0x17, 0x21);
const BORDER: u32 = rgb(0x2a, 0x33, 0x45);
const HOVER_BG: u32 = rgb(0x1e, 0x27, 0x38);
const TEXT: u32 = rgb(0xe8, 0xec, 0xf2);
const MUTED: u32 = rgb(0x8a, 0x93, 0xa3);
const ACCENT: u32 = rgb(0x2b, 0xc4, 0x8a);

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

/// Abre a thread da janela. `on_action` recebe os cliques nas linhas.
pub fn start(on_action: impl Fn(Action) + Send + Sync + 'static) {
    let _ = HANDLER.set(Box::new(on_action));
    std::thread::spawn(|| unsafe {
        let class = wide("OP2Card");
        let hinst = GetModuleHandleW(std::ptr::null());
        let wc = WNDCLASSW {
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(wndproc),
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: hinst,
            hIcon: std::ptr::null_mut(),
            hCursor: LoadCursorW(std::ptr::null_mut(), IDC_ARROW),
            hbrBackground: std::ptr::null_mut(),
            lpszMenuName: std::ptr::null(),
            lpszClassName: class.as_ptr(),
        };
        RegisterClassW(&wc);
        let hwnd = CreateWindowExW(
            WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE,
            class.as_ptr(),
            wide("OP2").as_ptr(),
            WS_POPUP,
            0,
            0,
            WIDTH,
            100,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            hinst,
            std::ptr::null(),
        );
        // cantos arredondados no Windows 11 (DWMWA_WINDOW_CORNER_PREFERENCE = ROUND)
        let round: i32 = 2;
        DwmSetWindowAttribute(hwnd, 33, &round as *const i32 as _, 4);
        HANDLE.store(hwnd as isize, Ordering::SeqCst);
        PostMessageW(hwnd, WM_UPDATE, 0, 0); // caso já tenha chegado algo antes da janela existir

        let mut msg: MSG = std::mem::zeroed();
        while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    });
}

fn refresh() {
    let h = HANDLE.load(Ordering::SeqCst);
    if h != 0 {
        unsafe { PostMessageW(h as HWND, WM_UPDATE, 0, 0) };
    }
}

pub fn show(view: View) {
    *VIEW.lock().unwrap() = Some(view);
    refresh();
}

pub fn hide() {
    *VIEW.lock().unwrap() = None;
    MOVED.store(false, Ordering::SeqCst);
    refresh();
}

fn scale(hwnd: HWND) -> f32 {
    let dpi = unsafe { GetDpiForWindow(hwnd) };
    if dpi == 0 { 1.0 } else { dpi as f32 / 96.0 }
}

fn px(v: i32, s: f32) -> i32 {
    (v as f32 * s).round() as i32
}

struct Fonts {
    title: HFONT,
    body: HFONT,
    small: HFONT,
    heading: HFONT,
}

impl Fonts {
    unsafe fn new(s: f32) -> Self {
        let face = wide("Segoe UI");
        let f = |size: i32, weight: i32| {
            CreateFontW(-px(size, s), 0, 0, 0, weight, 0, 0, 0, DEFAULT_CHARSET as u32, 0, 0, CLEARTYPE_QUALITY as u32, 0, face.as_ptr())
        };
        Self { title: f(16, 600), body: f(13, 400), small: f(12, 400), heading: f(11, 700) }
    }
}

impl Drop for Fonts {
    fn drop(&mut self) {
        unsafe {
            for f in [self.title, self.body, self.small, self.heading] {
                DeleteObject(f as HGDIOBJ);
            }
        }
    }
}

unsafe fn text(hdc: HDC, font: HFONT, color: u32, s: &str, mut r: RECT, flags: u32, paint: bool) -> i32 {
    SelectObject(hdc, font as HGDIOBJ);
    SetTextColor(hdc, color);
    let w: Vec<u16> = s.encode_utf16().collect();
    if flags & DT_WORDBREAK != 0 {
        let mut calc = r;
        DrawTextW(hdc, w.as_ptr(), w.len() as i32, &mut calc, flags | DT_CALCRECT);
        r.bottom = r.top + (calc.bottom - calc.top);
    }
    if paint {
        DrawTextW(hdc, w.as_ptr(), w.len() as i32, &mut r, flags);
    }
    r.bottom - r.top
}

unsafe fn fill(hdc: HDC, r: &RECT, color: u32, radius: i32) {
    let brush = CreateSolidBrush(color);
    let old_pen = SelectObject(hdc, GetStockObject(NULL_PEN));
    let old_brush = SelectObject(hdc, brush as HGDIOBJ);
    RoundRect(hdc, r.left, r.top, r.right + 1, r.bottom + 1, radius, radius);
    SelectObject(hdc, old_pen);
    SelectObject(hdc, old_brush);
    DeleteObject(brush as HGDIOBJ);
}

/// Desenha (ou só mede, com paint=false) o cartão. Retorna a altura total.
unsafe fn layout(hdc: HDC, v: &View, w: i32, s: f32, paint: bool, hover: Option<usize>, hits: &mut Vec<(RECT, Hit)>) -> i32 {
    let fonts = Fonts::new(s);
    SetBkMode(hdc, TRANSPARENT as _);
    let pad = px(14, s);
    let mut y = pad;
    let single = DT_SINGLELINE | DT_VCENTER | DT_NOPREFIX | DT_END_ELLIPSIS;
    let hovered = |hits: &Vec<(RECT, Hit)>| hover == Some(hits.len());

    // título + fechar
    let close = RECT { left: w - pad - px(18, s), top: y, right: w - pad + px(4, s), bottom: y + px(22, s) };
    if paint && hovered(hits) {
        fill(hdc, &close, HOVER_BG, px(6, s));
    }
    text(hdc, fonts.title, MUTED, "×", close, single | DT_CENTER, paint);
    hits.push((close, Hit::Close));
    let r = RECT { left: pad, top: y, right: close.left - px(4, s), bottom: y + px(22, s) };
    text(hdc, fonts.title, TEXT, &v.title, r, single, paint);
    y += px(24, s);
    if !v.subtitle.is_empty() {
        let r = RECT { left: pad, top: y, right: w - pad, bottom: y };
        y += text(hdc, fonts.small, ACCENT, &v.subtitle, r, DT_WORDBREAK | DT_NOPREFIX, paint);
    }

    for (heading, rows) in &v.sections {
        y += px(12, s);
        let r = RECT { left: pad, top: y, right: w - pad, bottom: y + px(16, s) };
        text(hdc, fonts.heading, MUTED, &heading.to_uppercase(), r, single, paint);
        y += px(18, s);
        for row in rows {
            let inner_left = pad + if row.marker.is_some() { px(16, s) } else { 0 };
            let h = if row.wrap {
                let r = RECT { left: inner_left, top: y + px(3, s), right: w - pad, bottom: y };
                text(hdc, fonts.small, TEXT, &row.text, r, DT_WORDBREAK | DT_NOPREFIX, false) + px(6, s)
            } else {
                px(26, s)
            };
            let full = RECT { left: pad - px(6, s), top: y, right: w - pad + px(6, s), bottom: y + h };
            if let Some(a) = &row.action {
                if paint && hovered(hits) {
                    fill(hdc, &full, HOVER_BG, px(8, s));
                }
                hits.push((full, Hit::Act(a.clone())));
            }
            if let Some(sel) = row.marker {
                let r = RECT { left: pad, top: y, right: pad + px(14, s), bottom: y + h };
                text(hdc, fonts.small, if sel { ACCENT } else { MUTED }, if sel { "●" } else { "○" }, r, single, paint);
            }
            if row.wrap {
                let r = RECT { left: inner_left, top: y + px(3, s), right: w - pad, bottom: y };
                text(hdc, fonts.small, TEXT, &row.text, r, DT_WORDBREAK | DT_NOPREFIX, paint);
            } else {
                let detail_w = if row.detail.is_empty() {
                    0
                } else {
                    SelectObject(hdc, fonts.small as HGDIOBJ);
                    let w16: Vec<u16> = row.detail.encode_utf16().collect();
                    let mut size: SIZE = std::mem::zeroed();
                    GetTextExtentPoint32W(hdc, w16.as_ptr(), w16.len() as i32, &mut size);
                    size.cx + px(10, s)
                };
                let r = RECT { left: inner_left, top: y, right: w - pad - detail_w, bottom: y + h };
                let color = if row.marker == Some(true) { ACCENT } else { TEXT };
                text(hdc, fonts.body, color, &row.text, r, single, paint);
                if detail_w > 0 {
                    let r = RECT { left: w - pad - detail_w, top: y, right: w - pad, bottom: y + h };
                    text(hdc, fonts.small, MUTED, &row.detail, r, single | DT_RIGHT, paint);
                }
            }
            y += h;
        }
    }

    if let Some(url) = &v.link {
        y += px(10, s);
        let r = RECT { left: pad - px(6, s), top: y, right: w - pad + px(6, s), bottom: y + px(26, s) };
        if paint && hovered(hits) {
            fill(hdc, &r, HOVER_BG, px(8, s));
        }
        let t = RECT { left: pad, ..r };
        text(hdc, fonts.body, ACCENT, "Abrir no OP.gg  ↗", t, single, paint);
        hits.push((r, Hit::Link(url.clone())));
        y += px(26, s);
    }
    y + pad
}

unsafe fn update(hwnd: HWND) {
    let view = VIEW.lock().unwrap().clone();
    let Some(v) = view else {
        ShowWindow(hwnd, SW_HIDE);
        return;
    };
    let s = scale(hwnd);
    let w = px(WIDTH, s);
    let hdc = GetDC(hwnd);
    let h = layout(hdc, &v, w, s, false, None, &mut Vec::new());
    ReleaseDC(hwnd, hdc);

    let mut flags = SWP_NOACTIVATE | SWP_SHOWWINDOW;
    let (mut x, mut y) = (0, 0);
    if MOVED.load(Ordering::SeqCst) {
        flags |= SWP_NOMOVE;
    } else {
        // canto direito da tela, um pouco acima do meio
        let mut work: RECT = std::mem::zeroed();
        SystemParametersInfoW(SPI_GETWORKAREA, 0, &mut work as *mut RECT as _, 0);
        x = work.right - w - px(16, s);
        y = (work.top + (work.bottom - work.top - h) / 2 - px(60, s)).max(work.top + px(16, s));
    }
    SetWindowPos(hwnd, HWND_TOPMOST, x, y, w, h, flags);
    InvalidateRect(hwnd, std::ptr::null(), 0);
}

unsafe fn paint(hwnd: HWND) {
    let mut ps: PAINTSTRUCT = std::mem::zeroed();
    let hdc = BeginPaint(hwnd, &mut ps);
    let mut rc: RECT = std::mem::zeroed();
    GetClientRect(hwnd, &mut rc);
    let (w, h) = (rc.right, rc.bottom);
    // desenha num bitmap e copia de uma vez (sem piscar)
    let mem = CreateCompatibleDC(hdc);
    let bmp = CreateCompatibleBitmap(hdc, w, h);
    let old = SelectObject(mem, bmp as HGDIOBJ);
    let bg = CreateSolidBrush(BG);
    FillRect(mem, &rc, bg);
    DeleteObject(bg as HGDIOBJ);
    let border = CreateSolidBrush(BORDER);
    FrameRect(mem, &rc, border);
    DeleteObject(border as HGDIOBJ);

    if let Some(v) = VIEW.lock().unwrap().clone() {
        let hover = *HOVER.lock().unwrap();
        let mut hits = Vec::new();
        layout(mem, &v, w, scale(hwnd), true, hover, &mut hits);
        *HITS.lock().unwrap() = hits;
    }
    BitBlt(hdc, 0, 0, w, h, mem, 0, 0, SRCCOPY);
    SelectObject(mem, old);
    DeleteObject(bmp as HGDIOBJ);
    DeleteDC(mem);
    EndPaint(hwnd, &ps);
}

fn hit_at(x: i32, y: i32) -> Option<(usize, Hit)> {
    let hits = HITS.lock().unwrap();
    hits.iter()
        .enumerate()
        .find(|(_, (r, _))| x >= r.left && x < r.right && y >= r.top && y < r.bottom)
        .map(|(i, (_, h))| (i, h.clone()))
}

fn lparam_xy(l: LPARAM) -> (i32, i32) {
    ((l & 0xffff) as i16 as i32, ((l >> 16) & 0xffff) as i16 as i32)
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_UPDATE => {
            update(hwnd);
            0
        }
        WM_PAINT => {
            paint(hwnd);
            0
        }
        WM_ERASEBKGND => 1,
        WM_MOUSEACTIVATE => MA_NOACTIVATE as LRESULT,
        WM_NCHITTEST => {
            // fora das linhas clicáveis, arrastar move o cartão
            let (sx, sy) = lparam_xy(lp);
            let mut p = POINT { x: sx, y: sy };
            ScreenToClient(hwnd, &mut p);
            if hit_at(p.x, p.y).is_some() { HTCLIENT as LRESULT } else { HTCAPTION as LRESULT }
        }
        WM_EXITSIZEMOVE => {
            MOVED.store(true, Ordering::SeqCst);
            0
        }
        WM_MOUSEMOVE => {
            let (x, y) = lparam_xy(lp);
            let now = hit_at(x, y).map(|(i, _)| i);
            let mut hover = HOVER.lock().unwrap();
            if *hover != now {
                *hover = now;
                InvalidateRect(hwnd, std::ptr::null(), 0);
            }
            let mut t = TRACKMOUSEEVENT { cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE, hwndTrack: hwnd, dwHoverTime: 0 };
            TrackMouseEvent(&mut t);
            0
        }
        WM_MOUSELEAVE => {
            *HOVER.lock().unwrap() = None;
            InvalidateRect(hwnd, std::ptr::null(), 0);
            0
        }
        WM_SETCURSOR => {
            let mut p: POINT = std::mem::zeroed();
            GetCursorPos(&mut p);
            ScreenToClient(hwnd, &mut p);
            let cursor = if hit_at(p.x, p.y).is_some() { IDC_HAND } else { IDC_ARROW };
            SetCursor(LoadCursorW(std::ptr::null_mut(), cursor));
            1
        }
        WM_LBUTTONUP => {
            let (x, y) = lparam_xy(lp);
            match hit_at(x, y).map(|(_, h)| h) {
                Some(Hit::Close) => hide(),
                Some(Hit::Link(url)) => {
                    ShellExecuteW(hwnd, wide("open").as_ptr(), wide(&url).as_ptr(), std::ptr::null(), std::ptr::null(), SW_SHOWNORMAL);
                }
                Some(Hit::Act(a)) => {
                    if let Some(h) = HANDLER.get() {
                        h(a);
                    }
                }
                None => {}
            }
            0
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}
