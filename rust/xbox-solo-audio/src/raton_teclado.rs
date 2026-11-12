//! A18: el ratón y el teclado del PC como mando de la consola. La
//! ruta nativa no tiene ratón ni teclado propios (la trama de entrada sólo lleva
//! mando), así que se emula el mando: WASD → stick izquierdo, ratón → stick
//! derecho, clics → gatillos, teclas → botones. Aquí, sólo la traducción (pura,
//! con pruebas); la captura de Windows y el envío van aparte.
//!
//! El mando es el bloque de 43 bytes de `nano::paquetes::Mando`: 1-16 un contador
//! por botón que sube en cada cambio, 17-18 gatillos, 19-26 ejes i16 (izquierdo
//! X e Y, derecho X e Y), 35-36 la máscara de botones (la de XInput).

use crate::nano::paquetes::{Mando, MANDO_BOTONES, MANDO_EJE_X};

/// Un botón del mando: (índice de su contador, bit en la máscara de XInput).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Boton { Arriba, Abajo, Izquierda, Derecha, Menu, Vista, StickIzq, StickDer, Lb, Rb, A, B, X, Y }

impl Boton {
    pub const TODOS: [Boton; 14] = [Self::Arriba, Self::Abajo, Self::Izquierda, Self::Derecha, Self::Menu, Self::Vista, Self::StickIzq, Self::StickDer, Self::Lb, Self::Rb, Self::A, Self::B, Self::X, Self::Y];
    pub fn bits(self) -> (usize, u16) {
        match self {
            Self::Arriba => (1, 0x0001), Self::Abajo => (2, 0x0002), Self::Izquierda => (3, 0x0004), Self::Derecha => (4, 0x0008),
            Self::Menu => (5, 0x0010), Self::Vista => (6, 0x0020), Self::StickIzq => (7, 0x0040), Self::StickDer => (8, 0x0080),
            Self::Lb => (9, 0x0100), Self::Rb => (10, 0x0200), Self::A => (13, 0x1000), Self::B => (14, 0x2000), Self::X => (15, 0x4000), Self::Y => (16, 0x8000),
        }
    }
}

/// Lo que hay pulsado o movido en el PC, ya traducido a intenciones de mando.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Estado {
    /// WASD: (derecha − izquierda, arriba − abajo), cada uno −1, 0 o 1.
    pub mover: (i8, i8),
    /// El ratón en este intervalo, en cuentas relativas de Raw Input.
    pub raton: (i32, i32),
    pub gatillo_izq: bool,
    pub gatillo_der: bool,
    pub botones: Vec<Boton>,
}

/// La traducción, con su configuración y lo que recuerda entre tramas.
pub struct Traductor {
    /// Cuentas de ratón por intervalo que llevan el stick al tope.
    pub sensibilidad: f32,
    /// Exponente de la curva del stick derecho (1 = lineal; <1 responde antes).
    pub curva: f32,
}

impl Default for Traductor {
    fn default() -> Self { Self { sensibilidad: 40.0, curva: 0.7 } }
}

const TOPE: f32 = 32767.0;

impl Traductor {
    /// Escribe el estado en el mando. Los contadores de los botones que cambian
    /// suben uno (como `sendButton` de greenlight-native); devuelve si algo cambió.
    pub fn aplicar(&mut self, e: &Estado, mando: &mut Mando) -> bool {
        let antes = *mando;
        let mascara = e.botones.iter().fold(0u16, |m, b| m | b.bits().1);
        let previos = u16::from_le_bytes([mando[MANDO_BOTONES], mando[MANDO_BOTONES + 1]]);
        for b in Boton::TODOS {
            let (contador, bit) = b.bits();
            if (mascara ^ previos) & bit != 0 { mando[contador] = mando[contador].wrapping_add(1) }
        }
        mando[MANDO_BOTONES..MANDO_BOTONES + 2].copy_from_slice(&mascara.to_le_bytes());
        mando[17] = if e.gatillo_izq { 255 } else { 0 };
        mando[18] = if e.gatillo_der { 255 } else { 0 };
        // Stick izquierdo: en diagonal, al tope en cada eje no; se normaliza.
        let (mx, my) = (f32::from(e.mover.0), f32::from(e.mover.1));
        let n = (mx * mx + my * my).sqrt().max(1.0);
        let ejes = [mx / n * TOPE, my / n * TOPE, self.stick(e.raton.0), self.stick(-e.raton.1)];
        for (i, v) in ejes.iter().enumerate() {
            let p = MANDO_EJE_X + 2 * i;
            mando[p..p + 2].copy_from_slice(&(v.round().clamp(-TOPE, TOPE) as i16).to_le_bytes());
        }
        *mando != antes
    }

    /// Movimiento del ratón (cuentas en el intervalo) → eje del stick derecho.
    fn stick(&self, cuentas: i32) -> f32 {
        let x = (cuentas as f32 / self.sensibilidad).clamp(-1.0, 1.0);
        x.signum() * x.abs().powf(self.curva) * TOPE
    }
}

#[cfg(test)]
mod pruebas {
    use super::*;

    fn eje(m: &Mando, i: usize) -> i16 { let p = MANDO_EJE_X + 2 * i; i16::from_le_bytes([m[p], m[p + 1]]) }

    #[test]
    fn botones_contadores_y_mascara() {
        let (mut t, mut m) = (Traductor::default(), [0u8; 43]);
        assert!(t.aplicar(&Estado { botones: vec![Boton::A, Boton::Arriba], ..Default::default() }, &mut m));
        assert_eq!(u16::from_le_bytes([m[35], m[36]]), 0x1001);
        assert_eq!((m[13], m[1], m[14]), (1, 1, 0), "sube el contador de lo que cambia");
        // Sigue pulsado: el contador no se mueve.
        assert!(!t.aplicar(&Estado { botones: vec![Boton::A, Boton::Arriba], ..Default::default() }, &mut m));
        assert_eq!(m[13], 1);
        // Se suelta A: su contador sube otra vez; la cruceta sigue.
        t.aplicar(&Estado { botones: vec![Boton::Arriba], ..Default::default() }, &mut m);
        assert_eq!((m[13], m[1], u16::from_le_bytes([m[35], m[36]])), (2, 1, 0x0001));
    }

    #[test]
    fn sticks_y_gatillos() {
        let (mut t, mut m) = (Traductor::default(), [0u8; 43]);
        t.aplicar(&Estado { mover: (1, 0), raton: (1000, -20), gatillo_der: true, ..Default::default() }, &mut m);
        assert_eq!((eje(&m, 0), eje(&m, 1)), (32767, 0));
        assert_eq!(eje(&m, 2), 32767, "mucho ratón: al tope");
        assert!(eje(&m, 3) > 0 && eje(&m, 3) < 32767, "ratón hacia arriba (y negativa) → stick arriba");
        assert_eq!((m[17], m[18]), (0, 255));
        // Diagonal: normalizada, no al tope en los dos.
        t.aplicar(&Estado { mover: (1, 1), ..Default::default() }, &mut m);
        assert!((eje(&m, 0) - 23170).abs() <= 1 && (eje(&m, 1) - 23170).abs() <= 1);
        // Quieto: todo a cero.
        t.aplicar(&Estado::default(), &mut m);
        assert_eq!((0..4).map(|i| eje(&m, i)).collect::<Vec<_>>(), vec![0, 0, 0, 0]);
    }

    #[test]
    fn el_raton_envia_una_ultima_trama_neutra_al_parar() {
        *CAPTURA.lock().unwrap() = Some(Captura { activa: true, dx: 12, ..Default::default() });
        assert_eq!(tomar().unwrap().raton, (12, 0));
        assert_eq!(tomar().unwrap().raton, (0, 0));
        assert!(tomar().is_none());
        *CAPTURA.lock().unwrap() = None;
    }

    #[test]
    fn deltas_crudos_relativos_sin_coordenadas_de_pantalla() {
        let mut c = Captura::default();
        c.acumular_raton(500, 300);
        assert_eq!((c.dx, c.dy), (0, 0), "sin captura no se consume el ratón");
        c.activa = true;
        c.acumular_raton(7, -3);
        c.acumular_raton(5, 2);
        assert_eq!((c.dx, c.dy), (12, -1));
    }
}

/// Lo capturado entre dos tramas: teclas pulsadas (códigos virtuales), botones
/// del ratón y su movimiento acumulado. F8 activa y suelta la captura.
#[derive(Default)]
struct Captura { activa: bool, teclas: std::collections::HashSet<u32>, izq: bool, der: bool, medio: bool, dx: i32, dy: i32, cambio: bool, hubo_raton: bool }

impl Captura {
    fn acumular_raton(&mut self, dx: i32, dy: i32) {
        if self.activa {
            self.dx = self.dx.saturating_add(dx);
            self.dy = self.dy.saturating_add(dy);
        }
    }
}

static CAPTURA: std::sync::Mutex<Option<Captura>> = std::sync::Mutex::new(None);
/// `--raton-teclado`: la sesión nativa mira la captura cada 8 ms.
pub static ACTIVADO: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
const F8: u32 = 0x77;

/// El estado para la trama siguiente (`None` si la captura está suelta o no
/// cambió nada). El movimiento del ratón se consume.
pub fn tomar() -> Option<Estado> {
    let mut g = CAPTURA.lock().ok()?;
    let c = g.as_mut()?;
    // Al soltar la captura, una vez el mando en reposo.
    if !c.activa { c.hubo_raton = false; return std::mem::take(&mut c.cambio).then(Estado::default) }
    let raton_movido = c.dx != 0 || c.dy != 0;
    if !(c.cambio || raton_movido || c.hubo_raton) { return None }
    let t = |vk: u32| c.teclas.contains(&vk);
    let mover = (i8::from(t(0x44)) - i8::from(t(0x41)), i8::from(t(0x57)) - i8::from(t(0x53)));
    let mapa: [(&[u32], Boton); 13] = [
        (&[0x20], Boton::A), (&[0x43, 0x11, 0xA2, 0xA3], Boton::B), (&[0x52], Boton::X), (&[0x45], Boton::Y),
        (&[0x51], Boton::Lb), (&[0x46], Boton::Rb), (&[0x09], Boton::Vista), (&[0x1B], Boton::Menu),
        (&[0x10, 0xA0, 0xA1], Boton::StickIzq), (&[0x56], Boton::StickDer),
        (&[0x26], Boton::Arriba), (&[0x28], Boton::Abajo), (&[0x25], Boton::Izquierda),
    ];
    let mut botones: Vec<Boton> = mapa.iter().filter(|(vks, _)| vks.iter().any(|v| t(*v))).map(|(_, b)| *b).collect();
    if t(0x27) { botones.push(Boton::Derecha) }
    if c.medio { botones.push(Boton::StickDer) }
    let e = Estado { mover, raton: (c.dx, c.dy), gatillo_izq: c.der, gatillo_der: c.izq, botones };
    (c.dx, c.dy, c.cambio) = (0, 0, false);
    c.hubo_raton = raton_movido;
    Some(e)
}

/// ¿Está la captura activa? (Al soltarla, la sesión manda el mando en reposo una vez.)
pub fn activa() -> bool { CAPTURA.lock().ok().and_then(|g| g.as_ref().map(|c| c.activa)).unwrap_or(false) }

/// Arranca la captura en su hilo (ganchos de bajo nivel de teclado y ratón con
/// su bucle de mensajes). Suelta hasta que se pulsa F8.
#[cfg(windows)]
pub fn capturar() {
    if let Ok(mut g) = CAPTURA.lock() { if g.is_some() { return } *g = Some(Captura::default()) }
    std::thread::Builder::new().name("raton-teclado".into()).spawn(|| unsafe {
        use windows::core::w;
        use windows::Win32::UI::HiDpi::{SetThreadDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2};
        use windows::Win32::UI::Input::{RegisterRawInputDevices, RAWINPUTDEVICE, RIDEV_INPUTSINK};
        use windows::Win32::UI::WindowsAndMessaging::*;
        // El hilo del gancho usa píxeles físicos para centrar; no cambia el DPI
        // del resto del proceso. WM_INPUT aporta deltas sin aceleración.
        let _ = SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let ventana = CreateWindowExW(WINDOW_EX_STYLE(0), w!("STATIC"), w!(""), WINDOW_STYLE(0), 0, 0, 0, 0, HWND_MESSAGE, None, None, None);
        if ventana.0 == 0 { return }
        let dispositivo = RAWINPUTDEVICE { usUsagePage: 1, usUsage: 2, dwFlags: RIDEV_INPUTSINK, hwndTarget: ventana };
        if RegisterRawInputDevices(&[dispositivo], std::mem::size_of::<RAWINPUTDEVICE>() as u32).is_err() {
            let _ = DestroyWindow(ventana);
            return
        }
        let _k = SetWindowsHookExW(WH_KEYBOARD_LL, Some(teclado), None, 0);
        let _r = SetWindowsHookExW(WH_MOUSE_LL, Some(raton), None, 0);
        let mut m = MSG::default();
        while GetMessageW(&mut m, None, 0, 0).as_bool() {
            if m.message == WM_INPUT && m.hwnd == ventana { entrada_cruda(m.lParam); }
            let _ = DispatchMessageW(&m);
        }
        let _ = DestroyWindow(ventana);
    }).ok();
}

#[cfg(not(windows))]
pub fn capturar() {}

/// Se ejecuta en el hilo de mensajes, fuera de los callbacks de los ganchos.
#[cfg(windows)]
unsafe fn entrada_cruda(l: windows::Win32::Foundation::LPARAM) {
    use windows::Win32::UI::Input::{GetRawInputData, HRAWINPUT, RAWINPUT, RAWINPUTHEADER, RID_INPUT, RIM_TYPEMOUSE, MOUSE_MOVE_ABSOLUTE};
    let mut bytes = 0u32;
    let handle = HRAWINPUT(l.0);
    let cabecera = std::mem::size_of::<RAWINPUTHEADER>() as u32;
    if GetRawInputData(handle, RID_INPUT, None, &mut bytes, cabecera) == u32::MAX || bytes < std::mem::size_of::<RAWINPUT>() as u32 { return }
    let mut buffer = vec![0u64; (bytes as usize).div_ceil(8)];
    if GetRawInputData(handle, RID_INPUT, Some(buffer.as_mut_ptr().cast()), &mut bytes, cabecera) == u32::MAX { return }
    let raw = &*(buffer.as_ptr().cast::<RAWINPUT>());
    if raw.header.dwType != RIM_TYPEMOUSE.0 { return }
    let mouse = raw.data.mouse;
    if mouse.usFlags.0 & MOUSE_MOVE_ABSOLUTE.0 == 0 {
        if let Ok(mut g) = CAPTURA.lock() {
            if let Some(c) = g.as_mut().filter(|c| c.activa) {
                c.acumular_raton(mouse.lLastX, mouse.lLastY);
            }
        }
    }
}

#[cfg(windows)]
fn centro() -> (i32, i32) {
    use windows::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_CXSCREEN, SM_CYSCREEN};
    unsafe { (GetSystemMetrics(SM_CXSCREEN) / 2, GetSystemMetrics(SM_CYSCREEN) / 2) }
}

#[cfg(windows)]
unsafe extern "system" fn teclado(codigo: i32, w: windows::Win32::Foundation::WPARAM, l: windows::Win32::Foundation::LPARAM) -> windows::Win32::Foundation::LRESULT {
    use windows::Win32::Foundation::LRESULT;
    use windows::Win32::UI::WindowsAndMessaging::*;
    if codigo >= 0 {
        let k = &*(l.0 as *const KBDLLHOOKSTRUCT);
        let abajo = matches!(w.0 as u32, WM_KEYDOWN | WM_SYSKEYDOWN);
        let mut tragar = false;
        if let Ok(mut g) = CAPTURA.lock() {
            if let Some(c) = g.as_mut() {
                if k.vkCode == F8 && abajo {
                    c.activa = !c.activa;
                    c.teclas.clear();
                    (c.izq, c.der, c.medio, c.cambio, c.dx, c.dy, c.hubo_raton) = (false, false, false, true, 0, 0, false);
                    if c.activa { let (x, y) = centro(); let _ = SetCursorPos(x, y); }
                    tragar = true;
                } else if c.activa {
                    let cambio = if abajo { c.teclas.insert(k.vkCode) } else { c.teclas.remove(&k.vkCode) };
                    c.cambio |= cambio;
                    tragar = true;
                }
            }
        }
        if tragar { return LRESULT(1) }
    }
    CallNextHookEx(None, codigo, w, l)
}

#[cfg(windows)]
unsafe extern "system" fn raton(codigo: i32, w: windows::Win32::Foundation::WPARAM, l: windows::Win32::Foundation::LPARAM) -> windows::Win32::Foundation::LRESULT {
    use windows::Win32::Foundation::LRESULT;
    use windows::Win32::UI::WindowsAndMessaging::*;
    if codigo >= 0 {
        let r = &*(l.0 as *const MSLLHOOKSTRUCT);
        if let Ok(mut g) = CAPTURA.lock() {
            if let Some(c) = g.as_mut() {
                if c.activa {
                    match w.0 as u32 {
                        // Sólo tragar y recentrar; WM_INPUT trae el delta sin
                        // aceleración ni mezcla de coordenadas lógicas/físicas.
                        WM_MOUSEMOVE if r.flags & LLMHF_INJECTED == 0 => {
                            let (x, y) = centro();
                            let _ = SetCursorPos(x, y);
                        }
                        WM_LBUTTONDOWN => { c.izq = true; c.cambio = true }
                        WM_LBUTTONUP => { c.izq = false; c.cambio = true }
                        WM_RBUTTONDOWN => { c.der = true; c.cambio = true }
                        WM_RBUTTONUP => { c.der = false; c.cambio = true }
                        WM_MBUTTONDOWN => { c.medio = true; c.cambio = true }
                        WM_MBUTTONUP => { c.medio = false; c.cambio = true }
                        _ => {}
                    }
                    return LRESULT(1);
                }
            }
        }
    }
    CallNextHookEx(None, codigo, w, l)
}
