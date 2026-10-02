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
    /// El ratón en este intervalo, en cuentas (Raw Input, relativo).
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
    previos: u16,
}

impl Default for Traductor {
    fn default() -> Self { Self { sensibilidad: 40.0, curva: 0.7, previos: 0 } }
}

const TOPE: f32 = 32767.0;

impl Traductor {
    /// Escribe el estado en el mando. Los contadores de los botones que cambian
    /// suben uno (como `sendButton` de greenlight-native); devuelve si algo cambió.
    pub fn aplicar(&mut self, e: &Estado, mando: &mut Mando) -> bool {
        let antes = *mando;
        let mascara = e.botones.iter().fold(0u16, |m, b| m | b.bits().1);
        for b in Boton::TODOS {
            let (contador, bit) = b.bits();
            if (mascara ^ self.previos) & bit != 0 { mando[contador] = mando[contador].wrapping_add(1) }
        }
        self.previos = mascara;
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
}

/// Lo capturado entre dos tramas: teclas pulsadas (códigos virtuales), botones
/// del ratón y su movimiento acumulado. F8 activa y suelta la captura.
#[derive(Default)]
struct Captura { activa: bool, teclas: std::collections::HashSet<u32>, izq: bool, der: bool, medio: bool, dx: i32, dy: i32, cambio: bool }

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
    if !c.activa { return std::mem::take(&mut c.cambio).then(Estado::default) }
    if !(c.cambio || c.dx != 0 || c.dy != 0) { return None }
    let t = |vk: u32| c.teclas.contains(&vk);
    let mover = (i8::from(t(0x44)) - i8::from(t(0x41)), i8::from(t(0x57)) - i8::from(t(0x53)));
    let mapa: [(&[u32], Boton); 13] = [
        (&[0x20], Boton::A), (&[0x43, 0x11, 0xA2], Boton::B), (&[0x52], Boton::X), (&[0x45], Boton::Y),
        (&[0x51], Boton::Lb), (&[0x46], Boton::Rb), (&[0x09], Boton::Vista), (&[0x1B], Boton::Menu),
        (&[0x10, 0xA0], Boton::StickIzq), (&[0x56], Boton::StickDer),
        (&[0x26], Boton::Arriba), (&[0x28], Boton::Abajo), (&[0x25], Boton::Izquierda),
    ];
    let mut botones: Vec<Boton> = mapa.iter().filter(|(vks, _)| vks.iter().any(|v| t(*v))).map(|(_, b)| *b).collect();
    if t(0x27) { botones.push(Boton::Derecha) }
    if c.medio { botones.push(Boton::StickDer) }
    let e = Estado { mover, raton: (c.dx, c.dy), gatillo_izq: c.der, gatillo_der: c.izq, botones };
    (c.dx, c.dy, c.cambio) = (0, 0, false);
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
        use windows::Win32::UI::WindowsAndMessaging::*;
        let _k = SetWindowsHookExW(WH_KEYBOARD_LL, Some(teclado), None, 0);
        let _r = SetWindowsHookExW(WH_MOUSE_LL, Some(raton), None, 0);
        let mut m = MSG::default();
        while GetMessageW(&mut m, None, 0, 0).as_bool() { let _ = DispatchMessageW(&m); }
    }).ok();
}

#[cfg(not(windows))]
pub fn capturar() {}

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
                    (c.izq, c.der, c.medio, c.cambio) = (false, false, false, true);
                    if c.activa { let (x, y) = centro(); let _ = SetCursorPos(x, y); }
                    crate::registro::registrar(std::time::Instant::now(), if c.activa { "ratón y teclado: CAPTURADOS para la consola (F8 los suelta)" } else { "ratón y teclado: sueltos" });
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
                        // El cursor no se mueve (se traga): la posición que traería, menos el centro, es el movimiento.
                        // El que provoca nuestro SetCursorPos (inyectado) no cuenta. Tras cada
                        // movimiento el cursor vuelve al centro: no se va al borde aunque Windows lo mueva.
                        WM_MOUSEMOVE if r.flags & LLMHF_INJECTED == 0 => {
                            let (x, y) = centro();
                            c.dx += r.pt.x - x;
                            c.dy += r.pt.y - y;
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
