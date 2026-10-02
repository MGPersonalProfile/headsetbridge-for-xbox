//! la caja negra de la sesión nativa. Ante «la consola dejó de
//! mandar» (5 s sin un paquete que se descifre, `PLAZO_DATOS`) el registro
//! decía sólo eso. Ahora guarda lo que entra y sale por el socket los últimos
//! segundos y, al cortarse, vuelca lo que pasó desde 2 s antes del último dato
//! hasta el corte: por cada 250 ms, lo recibido y lo mandado por tipo, y los
//! últimos sucesos uno a uno (errores de envío y recepción incluidos).

use std::collections::{BTreeMap, VecDeque};
use std::time::{Duration, Instant};

/// Lo que se guarda: el corte se declara a los 5 s sin datos y se vuelca desde
/// 2 s antes del último, así que 8 s bastan.
const GUARDAR: Duration = Duration::from_secs(8);
pub const ANTES: Duration = Duration::from_secs(2);
const TRAMO: Duration = Duration::from_millis(250);
const UNO_A_UNO: usize = 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Suceso {
    /// RTP de la consola que se descifra (tipo, SSRC).
    Recibido(u8, u32),
    /// RTP de la consola que no se descifra con la clave actual.
    SinDescifrar(u8),
    /// STUN de la consola (tipo del mensaje).
    StunRecibido(u16),
    /// Algo desde otra dirección (ni la consola ni STUN).
    Ajeno,
    ErrorRecibir,
    /// RTP nuestro (tipo, SSRC); `false` si `send_to` falló.
    Mandado(u8, u32, bool),
    /// STUN nuestro; `false` si `send_to` falló.
    StunMandado(bool),
}

impl Suceso {
    fn nombre(&self) -> String {
        match *self {
            Suceso::Recibido(pt, ssrc) => format!("← RTP {pt}/{ssrc}"),
            Suceso::SinDescifrar(pt) => format!("← RTP {pt} SIN DESCIFRAR"),
            Suceso::StunRecibido(t) => format!("← STUN {t:#06x}"),
            Suceso::Ajeno => "← de otra dirección".into(),
            Suceso::ErrorRecibir => "← ERROR al recibir".into(),
            Suceso::Mandado(pt, ssrc, ok) => format!("→ RTP {pt}/{ssrc}{}", if ok { "" } else { " ERROR al mandar" }),
            Suceso::StunMandado(ok) => format!("→ STUN{}", if ok { "" } else { " ERROR al mandar" }),
        }
    }
}

#[derive(Default)]
pub struct Caja { sucesos: VecDeque<(Instant, Suceso)> }

impl Caja {
    pub fn anotar(&mut self, s: Suceso) { self.anotar_en(Instant::now(), s) }

    pub fn anotar_en(&mut self, t: Instant, s: Suceso) {
        while self.sucesos.front().is_some_and(|(v, _)| t.duration_since(*v) > GUARDAR) { self.sucesos.pop_front(); }
        self.sucesos.push_back((t, s));
    }

    /// Las líneas para el registro: desde `ANTES` antes de `ultimo_dato` hasta `ahora`.
    pub fn volcar(&self, ultimo_dato: Instant, ahora: Instant) -> Vec<String> {
        let desde = ultimo_dato.checked_sub(ANTES).unwrap_or(ultimo_dato);
        let tramo: Vec<&(Instant, Suceso)> = self.sucesos.iter().filter(|(t, _)| *t >= desde && *t <= ahora).collect();
        let ms = |t: Instant| if t >= ultimo_dato { t.duration_since(ultimo_dato).as_millis() as i64 } else { -(ultimo_dato.duration_since(t).as_millis() as i64) };
        let mut lineas = vec![format!("caja negra: {} sucesos desde {} ms antes del último dato hasta el corte (+{} ms); en ms respecto al último dato",
            tramo.len(), ANTES.as_millis(), ahora.duration_since(ultimo_dato).as_millis())];
        // Por tramos de 250 ms: cuántos de cada clase (los tramos vacíos se cuentan juntos).
        let mut vacios = 0u32;
        let mut inicio = desde;
        while inicio < ahora {
            let fin = inicio + TRAMO;
            let mut cuenta: BTreeMap<String, u32> = BTreeMap::new();
            for (_, s) in tramo.iter().filter(|(t, _)| *t >= inicio && *t < fin) { *cuenta.entry(s.nombre()).or_default() += 1 }
            if cuenta.is_empty() { vacios += 1 } else {
                if vacios > 0 { lineas.push(format!("caja negra:   {} ms sin nada", vacios as u128 * TRAMO.as_millis())); vacios = 0 }
                lineas.push(format!("caja negra:   [{:+}] {}", ms(inicio), cuenta.iter().map(|(n, c)| format!("{n} ×{c}")).collect::<Vec<_>>().join(", ")));
            }
            inicio = fin;
        }
        if vacios > 0 { lineas.push(format!("caja negra:   {} ms sin nada hasta el corte", vacios as u128 * TRAMO.as_millis())) }
        let ultimos: Vec<String> = tramo.iter().rev().take(UNO_A_UNO).rev().map(|(t, s)| format!("{:+} {}", ms(*t), s.nombre())).collect();
        lineas.push(format!("caja negra: los últimos {}: {}", ultimos.len(), ultimos.join(" · ")));
        lineas
    }
}

#[cfg(test)]
mod pruebas {
    use super::*;

    #[test]
    fn guarda_ocho_segundos_y_vuelca_desde_dos_antes() {
        let t = Instant::now();
        let mut c = Caja::default();
        // 10 s de audio cada 20 ms y un keepalive nuestro por segundo; luego, nada.
        for i in 0..500u64 { c.anotar_en(t + Duration::from_millis(i * 20), Suceso::Recibido(35, 2)) }
        for s in 0..10u64 { c.anotar_en(t + Duration::from_secs(s), Suceso::Mandado(101, 0, s != 9)) }
        let ultimo = t + Duration::from_millis(499 * 20);
        c.anotar_en(ultimo + Duration::from_secs(3), Suceso::StunRecibido(1));
        let ahora = ultimo + Duration::from_millis(5100);
        c.anotar_en(ahora, Suceso::Mandado(101, 0, true));
        assert!(c.sucesos.front().unwrap().0 >= ahora - GUARDAR);
        let v = c.volcar(ultimo, ahora);
        assert!(v[0].contains("sucesos desde 2000 ms"), "{v:?}");
        assert!(v.iter().any(|l| l.contains("[-2000] ← RTP 35/2 ×13")), "{v:?}");
        assert!(v.iter().any(|l| l.contains("→ RTP 101/0 ERROR al mandar ×1")), "{v:?}");
        assert!(v.iter().any(|l| l.contains("ms sin nada")), "{v:?}");
        let ultimos = v.last().unwrap();
        assert!(ultimos.contains("+3000 ← STUN 0x0001") && ultimos.ends_with("+5100 → RTP 101/0"), "{ultimos}");
    }
}
