//! SRTP AES-128-GCM de la ruta Nano V3 nativa, igual que
//! `greenlight-native/packages/rtp/src/srtpcrypto.ts`: claves de sesión por
//! AES-CTR (RFC 3711 §4.3, índice 0 cifrado, 2 sal) y por paquete un IV de 12
//! bytes = ((ssrc << 48) + índice) XOR sal[2..14]; el AAD es la cabecera RTP de
//! 12 bytes y la etiqueta (16 bytes) va al final de la carga.

use aes::cipher::{KeyIvInit, StreamCipher};
use aes_gcm::aead::{AeadInPlace, KeyInit};
use aes_gcm::{Aes128Gcm, Nonce};
use base64::Engine;

pub const CABECERA: usize = 12;

pub struct Srtp {
    cifrador: Aes128Gcm,
    sal: u128,
}

#[derive(Debug, PartialEq)]
pub struct ErrorSrtp(pub &'static str);

impl std::fmt::Display for ErrorSrtp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { f.write_str(self.0) }
}
impl std::error::Error for ErrorSrtp {}

/// Clave de sesión `indice` (0 cifrado, 1 autenticación, 2 sal), `largo` bytes.
fn derivar(maestra: &[u8; 16], sal_maestra: &[u8; 14], indice: u128, largo: usize) -> Vec<u8> {
    let sal = sal_maestra.iter().fold(0u128, |a, b| (a << 8) | u128::from(*b));
    let iv = ((sal ^ (indice << 48)) << 16).to_be_bytes();
    let mut bloque = [0u8; 16];
    ctr::Ctr128BE::<aes::Aes128>::new(maestra.into(), &iv.into()).apply_keystream(&mut bloque);
    bloque[..largo].to_vec()
}

impl Srtp {
    /// `clave`: `serverDetails.srtp.key` de /configuration (base64, 30 bytes).
    pub fn nuevo(clave: &str) -> Result<Self, ErrorSrtp> {
        let bytes = base64::engine::general_purpose::STANDARD.decode(clave.trim()).map_err(|_| ErrorSrtp("la clave SRTP no es base64"))?;
        let (maestra, sal) = bytes.split_at_checked(16).ok_or(ErrorSrtp("clave SRTP corta"))?;
        let maestra: [u8; 16] = maestra.try_into().unwrap();
        let sal: [u8; 14] = sal.try_into().map_err(|_| ErrorSrtp("la clave SRTP no mide 30 bytes"))?;
        let cifrado = derivar(&maestra, &sal, 0, 16);
        let sal_sesion = derivar(&maestra, &sal, 2, 14);
        Ok(Self {
            cifrador: Aes128Gcm::new_from_slice(&cifrado).expect("16 bytes"),
            // Los 12 últimos bytes de la sal de sesión.
            sal: sal_sesion[2..].iter().fold(0u128, |a, b| (a << 8) | u128::from(*b)),
        })
    }

    fn iv(&self, cabecera: &[u8], roc: u32) -> [u8; 12] {
        let ssrc = u128::from(u32::from_be_bytes(cabecera[8..12].try_into().unwrap()));
        let secuencia = u128::from(u16::from_be_bytes([cabecera[2], cabecera[3]]));
        let indice = secuencia + (u128::from(roc) << 16);
        let v = ((ssrc << 48) + indice) ^ self.sal;
        v.to_be_bytes()[4..].try_into().unwrap()
    }

    /// Paquete RTP cifrado → carga en claro.
    pub fn descifrar(&self, paquete: &[u8], roc: u32) -> Result<Vec<u8>, ErrorSrtp> {
        let mut claro = Vec::new();
        self.descifrar_en(paquete, roc, &mut claro)?;
        Ok(claro)
    }

    /// lo mismo en un búfer que se reutiliza (sin asignar por paquete).
    pub fn descifrar_en(&self, paquete: &[u8], roc: u32, claro: &mut Vec<u8>) -> Result<(), ErrorSrtp> {
        if paquete.len() < CABECERA + 16 { return Err(ErrorSrtp("paquete SRTP corto")) }
        let (cabecera, cifrada) = paquete.split_at(CABECERA);
        claro.clear();
        claro.extend_from_slice(cifrada);
        self.cifrador.decrypt_in_place(Nonce::from_slice(&self.iv(cabecera, roc)), cabecera, claro)
            .map_err(|_| ErrorSrtp("etiqueta SRTP inválida"))
    }

    /// Cabecera RTP de 12 bytes + carga en claro → paquete cifrado (una sola
    /// asignación: se cifra en el sitio, detrás de la cabecera).
    pub fn cifrar(&self, cabecera: &[u8; CABECERA], carga: &[u8], roc: u32) -> Vec<u8> {
        let mut paquete = Vec::with_capacity(CABECERA + carga.len() + 16);
        paquete.extend_from_slice(cabecera);
        paquete.extend_from_slice(carga);
        let etiqueta = self.cifrador.encrypt_in_place_detached(Nonce::from_slice(&self.iv(cabecera, roc)), cabecera, &mut paquete[CABECERA..])
            .expect("AES-GCM no falla al cifrar");
        paquete.extend_from_slice(&etiqueta);
        paquete
    }
}

/// Cabecera RTP: versión 2, sin relleno, extensión ni CSRC, marca a 0 y
/// marca de tiempo a 0, como la manda greenlight-native.
pub fn cabecera_rtp(tipo: u8, secuencia: u16, ssrc: u32) -> [u8; CABECERA] {
    let mut c = [0u8; CABECERA];
    c[0] = 0x80;
    c[1] = tipo & 0x7f;
    c[2..4].copy_from_slice(&secuencia.to_be_bytes());
    c[8..12].copy_from_slice(&ssrc.to_be_bytes());
    c
}

/// (tipo de carga sin la marca, secuencia, ssrc) de un paquete RTP.
pub fn leer_cabecera(paquete: &[u8]) -> Option<(u8, u16, u32)> {
    if paquete.len() < CABECERA || paquete[0] >> 6 != 2 { return None }
    Some((paquete[1] & 0x7f, u16::from_be_bytes([paquete[2], paquete[3]]), u32::from_be_bytes(paquete[8..12].try_into().ok()?)))
}

#[cfg(test)]
mod pruebas {
    use super::*;

    const RTP_SEQUENCE: &[u8] = include_bytes!("../../pruebas/nano/rtp_sequence.bin");
    const RTP_INIT: &[u8] = include_bytes!("../../pruebas/nano/rtp_init.bin");

    /// cuánto cuesta descifrar un paquete con y sin asignar
    /// (`cargo test --release -- --ignored medir_descifrado --nocapture`).
    #[test]
    #[ignore]
    fn medir_descifrado() {
        let srtp = Srtp::nuevo("tnDk9YF5trmz6Ujoe4+/5RRaYpwCWXMhfSkvhGHG").unwrap();
        let carga = vec![0x5a; 300];
        let paquete = srtp.cifrar(&cabecera_rtp(35, 7, 1027), &carga, 0);
        let n = 200_000;
        let t = std::time::Instant::now();
        for _ in 0..n { std::hint::black_box(srtp.descifrar(std::hint::black_box(&paquete), 0).unwrap()); }
        let con = t.elapsed().as_nanos() / n;
        let mut claro = Vec::with_capacity(2048);
        let t = std::time::Instant::now();
        for _ in 0..n { srtp.descifrar_en(std::hint::black_box(&paquete), 0, &mut claro).unwrap(); std::hint::black_box(&claro); }
        let sin = t.elapsed().as_nanos() / n;
        let t = std::time::Instant::now();
        for _ in 0..n { std::hint::black_box(srtp.cifrar(&cabecera_rtp(35, 7, 1027), std::hint::black_box(&carga), 0)); }
        let cifrar = t.elapsed().as_nanos() / n;
        println!("paquete de {} B: descifrar {con} ns (asignando), {sin} ns (en el búfer); cifrar {cifrar} ns", paquete.len());
    }
    const RTP_ROC: &[u8] = include_bytes!("../../pruebas/nano/rtp_roc.bin");

    fn hex(s: &str) -> Vec<u8> { (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect() }

    #[test]
    fn descifra_y_cifra_rtp_sequence() {
        let srtp = Srtp::nuevo("tnDk9YF5trmz6Ujoe4+/5RRaYpwCWXMhfSkvhGHG").unwrap();
        assert_eq!(&RTP_SEQUENCE[CABECERA..], hex("55fe006ce26a73b5699e0d96cae4772ad5b71e441a89").as_slice());
        assert_eq!(srtp.descifrar(RTP_SEQUENCE, 0).unwrap(), hex("020066050000"));
        let cabecera: [u8; CABECERA] = RTP_SEQUENCE[..CABECERA].try_into().unwrap();
        assert_eq!(srtp.cifrar(&cabecera, &hex("020066050000"), 0), RTP_SEQUENCE);
    }

    #[test]
    fn descifra_rtp_init() {
        let srtp = Srtp::nuevo("tnDk9YF5trmz6Ujoe4+/5RRaYpwCWXMhfSkvhGHG").unwrap();
        let claro = srtp.descifrar(RTP_INIT, 0).unwrap();
        assert!(claro.starts_with(&hex("01000100000000000000f8")));
        assert_eq!(claro.len(), RTP_INIT.len() - CABECERA - 16);
    }

    #[test]
    fn descifra_con_roc() {
        let srtp = Srtp::nuevo("dEFX8nA5GtV/y/j36w4zl4K34YDNSB+JhhVoSI3h").unwrap();
        assert!(srtp.descifrar(RTP_ROC, 1).is_ok());
        assert!(srtp.descifrar(RTP_ROC, 0).is_err());
    }

    #[test]
    fn cabecera_de_ida_y_vuelta() {
        let c = cabecera_rtp(102, 13, 0);
        assert_eq!(leer_cabecera(&c), Some((102, 13, 0)));
    }
}

#[cfg(test)]
mod volcado {
    #[test]
    #[ignore]
    fn volcar() {
        let srtp = super::Srtp::nuevo("vV9cuxwCpZ2iKGVFJhdLBcQ2mfSRzFvPj7+vTQbq").unwrap();
        let mut nombres: Vec<_> = std::fs::read_dir("pruebas/nano").unwrap().map(|e| e.unwrap().path()).collect();
        nombres.sort();
        for n in nombres {
            let b = std::fs::read(&n).unwrap();
            let (pt, seq, ssrc) = super::leer_cabecera(&b).unwrap();
            match srtp.descifrar(&b, 0) {
                Ok(c) => println!("{} pt={pt} m={} seq={seq} ssrc={ssrc} {}", n.file_name().unwrap().to_string_lossy(), b[1] >> 7, c.iter().map(|x| format!("{x:02x}")).collect::<String>()),
                Err(e) => println!("{} {e}", n.display()),
            }
        }
    }
}
