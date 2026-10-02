//! cuándo se vuelve a intentar tras un corte. Antes, a los 5
//! intentos (1-2-4-8-16 s) el exe se rendía: con la consola apagada o la Wi-Fi
//! caída más de ~30 s, al volver ya no sonaba. Ahora, tras los rápidos, se
//! espera sin rendirse mirando cada 30 s si la consola está encendida.

use std::time::Duration;

/// Intentos seguidos a 1, 2, 4, 8 y 16 s.
pub const RAPIDOS: u32 = 5;
/// Luego, cada cuánto se mira si la consola está encendida.
pub const LENTO: Duration = Duration::from_secs(30);

#[derive(Debug, PartialEq)]
pub enum Espera {
    /// Otro intento tras esto.
    Rapida(Duration),
    /// Esperando a la consola: mirar cada `LENTO` y volver a intentar al verla.
    Lenta,
}

/// `reintentos`: los fallos seguidos, contando éste (1 el primero).
pub fn espera(reintentos: u32) -> Espera {
    match reintentos { 1..=RAPIDOS => Espera::Rapida(Duration::from_secs(1 << (reintentos - 1))), _ => Espera::Lenta }
}

#[cfg(test)]
mod pruebas {
    use super::*;

    #[test]
    fn calendario_de_reintentos() {
        let rapidas: Vec<u64> = (1..=5).map(|n| match espera(n) { Espera::Rapida(d) => d.as_secs(), Espera::Lenta => 0 }).collect();
        assert_eq!(rapidas, vec![1, 2, 4, 8, 16]);
        // Y luego, sin rendirse nunca.
        for n in [6, 7, 100, u32::MAX] { assert_eq!(espera(n), Espera::Lenta) }
    }
}
