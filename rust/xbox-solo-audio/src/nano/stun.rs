//! STUN mínimo para el ICE activo de la ruta nativa (RFC 5389 y 8445):
//! comprobaciones BINDING del cliente (controlador) y respuestas a las de la
//! consola, con MESSAGE-INTEGRITY (HMAC-SHA1) y FINGERPRINT.

use hmac::{Hmac, Mac};
use sha1::Sha1;
use std::net::SocketAddr;

const MAGIA: u32 = 0x2112_a442;
pub const PETICION: u16 = 0x0001;
pub const EXITO: u16 = 0x0101;

const MAPPED_ADDRESS: u16 = 0x0001;
const USERNAME: u16 = 0x0006;
const INTEGRIDAD: u16 = 0x0008;
const XOR_MAPPED_ADDRESS: u16 = 0x0020;
const PRIORITY: u16 = 0x0024;
const USE_CANDIDATE: u16 = 0x0025;
const FINGERPRINT: u16 = 0x8028;
const ICE_CONTROLLING: u16 = 0x802a;
const ICE_CONTROLLED: u16 = 0x8029;

const CRC32: crc::Crc<u32> = crc::Crc::<u32>::new(&crc::CRC_32_ISO_HDLC);

/// ¿Es STUN? Los dos bits altos a 0 y la cookie mágica (RTP empieza por 0x80).
pub fn es_stun(b: &[u8]) -> bool {
    b.len() >= 20 && b[0] & 0xc0 == 0 && u32::from_be_bytes([b[4], b[5], b[6], b[7]]) == MAGIA
}

struct Mensaje { b: Vec<u8>, relleno: u8 }

impl Mensaje {
    fn nuevo(tipo: u16, transaccion: &[u8; 12]) -> Self {
        let mut b = Vec::with_capacity(128);
        b.extend_from_slice(&tipo.to_be_bytes());
        b.extend_from_slice(&[0, 0]);
        b.extend_from_slice(&MAGIA.to_be_bytes());
        b.extend_from_slice(transaccion);
        Self { b, relleno: 0 }
    }
    fn fijar_largo(&mut self, extra: usize) {
        let largo = (self.b.len() - 20 + extra) as u16;
        self.b[2..4].copy_from_slice(&largo.to_be_bytes());
    }
    fn atributo(mut self, tipo: u16, valor: &[u8]) -> Self {
        self.b.extend_from_slice(&tipo.to_be_bytes());
        self.b.extend_from_slice(&(valor.len() as u16).to_be_bytes());
        self.b.extend_from_slice(valor);
        while self.b.len() % 4 != 0 { self.b.push(self.relleno) }
        self.fijar_largo(0);
        self
    }
    /// MESSAGE-INTEGRITY: HMAC-SHA1 con la contraseña (credencial de corto plazo)
    /// sobre el mensaje con el largo que ya cuenta este atributo.
    fn integridad(mut self, clave: &str) -> Self {
        self.fijar_largo(24);
        let mut mac = <Hmac<Sha1>>::new_from_slice(clave.as_bytes()).expect("HMAC acepta cualquier clave");
        mac.update(&self.b);
        let firma = mac.finalize().into_bytes();
        self.atributo(INTEGRIDAD, &firma)
    }
    fn huella(mut self) -> Vec<u8> {
        self.fijar_largo(8);
        let crc = CRC32.checksum(&self.b) ^ 0x5354_554e;
        self.atributo(FINGERPRINT, &crc.to_be_bytes()).b
    }
}

/// Comprobación del cliente controlador: USERNAME «remoto:local», PRIORITY,
/// ICE-CONTROLLING, USE-CANDIDATE, integridad con la contraseña REMOTA y huella.
pub fn comprobacion(transaccion: &[u8; 12], usuario: &str, prioridad: u32, desempate: u64, clave_remota: &str) -> Vec<u8> {
    Mensaje::nuevo(PETICION, transaccion)
        .atributo(USERNAME, usuario.as_bytes())
        .atributo(PRIORITY, &prioridad.to_be_bytes())
        .atributo(ICE_CONTROLLING, &desempate.to_be_bytes())
        .atributo(USE_CANDIDATE, &[])
        .integridad(clave_remota)
        .huella()
}

/// Valor de MAPPED-ADDRESS (sin XOR) o XOR-MAPPED-ADDRESS.
fn direccion(origen: SocketAddr, transaccion: &[u8; 12], xor: bool) -> Vec<u8> {
    let mut valor = vec![0u8];
    let puerto = if xor { origen.port() ^ (MAGIA >> 16) as u16 } else { origen.port() };
    let mut clave = MAGIA.to_be_bytes().to_vec();
    clave.extend_from_slice(transaccion);
    if !xor { clave = vec![0; 16] }
    match origen {
        SocketAddr::V4(a) => {
            valor.push(1);
            valor.extend_from_slice(&puerto.to_be_bytes());
            valor.extend(a.ip().octets().iter().zip(&clave).map(|(x, k)| x ^ k));
        }
        SocketAddr::V6(a) => {
            valor.push(2);
            valor.extend_from_slice(&puerto.to_be_bytes());
            valor.extend(a.ip().octets().iter().zip(&clave).map(|(x, k)| x ^ k));
        }
    }
    valor
}

/// Respuesta de éxito a una comprobación de la consola, como greenlight-native
/// (`packages/stun/src/index.ts`, `onBindingRequest`): MAPPED-ADDRESS,
/// XOR-MAPPED-ADDRESS e integridad con la contraseña REMOTA, sin huella. La
/// RFC pide la local, pero con ella la consola calla tras el ICE (medido el
/// 01-10 por la sesión local: «la consola dejó de mandar» a los 5 s).
pub fn respuesta(transaccion: &[u8; 12], origen: SocketAddr, clave_remota: &str) -> Vec<u8> {
    Mensaje::nuevo(EXITO, transaccion)
        .atributo(MAPPED_ADDRESS, &direccion(origen, transaccion, false))
        .atributo(XOR_MAPPED_ADDRESS, &direccion(origen, transaccion, true))
        .integridad(clave_remota).b
}

/// El rol ICE que declara una petición, con su desempate.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Rol { Controlling(u64), Controlled(u64) }

pub struct Leido {
    pub tipo: u16, pub transaccion: [u8; 12], pub usuario: Option<String>,
    pub rol: Option<Rol>, pub use_candidate: bool, pub integridad: bool,
}

pub fn leer(b: &[u8]) -> Option<Leido> {
    if !es_stun(b) { return None }
    let tipo = u16::from_be_bytes([b[0], b[1]]);
    let largo = usize::from(u16::from_be_bytes([b[2], b[3]]));
    let fin = (20 + largo).min(b.len());
    let (mut usuario, mut rol, mut use_candidate, mut integridad) = (None, None, false, false);
    let mut pos = 20;
    while pos + 4 <= fin {
        let t = u16::from_be_bytes([b[pos], b[pos + 1]]);
        let n = usize::from(u16::from_be_bytes([b[pos + 2], b[pos + 3]]));
        let valor = b.get(pos + 4..pos + 4 + n)?;
        let u64_de = |v: &[u8]| v.try_into().ok().map(u64::from_be_bytes);
        match t {
            USERNAME => usuario = std::str::from_utf8(valor).ok().map(str::to_owned),
            ICE_CONTROLLING => rol = u64_de(valor).map(Rol::Controlling),
            ICE_CONTROLLED => rol = u64_de(valor).map(Rol::Controlled),
            USE_CANDIDATE => use_candidate = true,
            INTEGRIDAD => integridad = true,
            _ => {}
        }
        pos += 4 + n.div_ceil(4) * 4;
    }
    Some(Leido { tipo, transaccion: b[8..20].try_into().ok()?, usuario, rol, use_candidate, integridad })
}

#[cfg(test)]
mod pruebas {
    use super::*;

    fn hex(s: &str) -> Vec<u8> {
        let s: String = s.split_whitespace().collect();
        (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
    }

    /// RFC 5769 §2.1 (la petición de ejemplo, con relleno de espacios).
    #[test]
    fn integridad_y_huella_de_rfc5769() {
        let esperado = hex("0001 0058 2112a442 b7e7a701bc34d686fa87dfae
            8022 0010 5354554e2074657374 20636c69656e74
            0024 0004 6e0001ff
            8029 0008 932ff9b151263b36
            0006 0009 6576746a3a68367659 202020
            0008 0014 9aeaa70cbfd8cb56781ef2b5b2d3f249c1b571a2
            8028 0004 e57a3bcf");
        let t: [u8; 12] = esperado[8..20].try_into().unwrap();
        let mut m = Mensaje::nuevo(PETICION, &t);
        m.relleno = 0x20;
        let b = m.atributo(0x8022, b"STUN test client").atributo(PRIORITY, &0x6e00_01ffu32.to_be_bytes())
            .atributo(0x8029, &hex("932ff9b151263b36")).atributo(USERNAME, b"evtj:h6vY")
            .integridad("VOkJxbRl1RmTxUk/WvJxBt").huella();
        assert_eq!(b, esperado);
        let l = leer(&b).unwrap();
        assert_eq!((l.tipo, l.usuario.as_deref()), (PETICION, Some("evtj:h6vY")));
        assert!(l.integridad);
    }

    /// RFC 5769 §2.2: XOR-MAPPED-ADDRESS de 192.0.2.1:32853.
    #[test]
    fn direccion_xor() {
        let t = [0u8; 12];
        let r = respuesta(&t, "192.0.2.1:32853".parse().unwrap(), "x");
        assert_eq!(&r[20..32], &hex("0001 0008 00018055c0000201")[..]);
        assert_eq!(&r[32..44], &hex("0020 0008 0001a147e112a643")[..]);
        // Termina en MESSAGE-INTEGRITY (sin huella), como greenlight-native.
        assert_eq!(&r[r.len() - 24..r.len() - 20], &hex("0008 0014")[..]);
        assert!(es_stun(&r) && !es_stun(&[0x80; 40]));
    }

    #[test]
    fn rol_de_la_comprobacion() {
        let c = leer(&comprobacion(&[1; 12], "a:b", 1, 0x1234, "x")).unwrap();
        assert_eq!((c.rol, c.use_candidate, c.integridad), (Some(Rol::Controlling(0x1234)), true, true));
    }

    #[test]
    fn comprobacion_completa() {
        let c = comprobacion(&[7; 12], "remoto:local", 2_130_705_919, 0x89ec_5f41_7241_e2d2, "clave");
        let l = leer(&c).unwrap();
        assert_eq!((l.tipo, l.usuario.as_deref(), c.len() % 4), (PETICION, Some("remoto:local"), 0));
        // La huella va al final y cuadra.
        let n = c.len();
        assert_eq!(u32::from_be_bytes(c[n - 4..].try_into().unwrap()), CRC32.checksum(&c[..n - 8]) ^ 0x5354_554e);
    }
}
