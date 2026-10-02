//! Lo común al relevo de sesiones: varias
//! sesiones a la vez, una sola reserva. La más nueva que da audio se queda la
//! reserva; lo que aún llegue de las viejas se tira.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use crate::audio::ReservaCompartida;
use crate::registro::registrar;
use crate::reserva::Pausa;

tokio::task_local! {
    /// Etiqueta de la sesión en el registro («[#2] ») durante un relevo.
    pub static ETIQUETA: String;
}

/// Registra con la etiqueta de la sesión, si la hay.
pub fn reg(t0: Instant, texto: &str) {
    let e = ETIQUETA.try_with(|e| e.clone()).unwrap_or_default();
    registrar(t0, &format!("{e}{texto}"));
}

/// La etiqueta de un número de sesión (vacía fuera del relevo).
pub fn etiqueta(numero: u64) -> String { if numero == 0 { String::new() } else { format!("[#{numero}] ") } }

/// los hitos del arranque de cada sesión, para la línea resumen
/// del relevo y para relevar a S s de Provisioned (`--relevo-provisioned`).
#[derive(Default, Clone, Copy)]
pub struct Hitos { pub play: Option<Instant>, pub provisioned: Option<Instant>, pub configuracion: Option<Instant>, pub ice: Option<Instant>, pub conectado: Option<Instant> }

static HITOS: std::sync::Mutex<std::collections::BTreeMap<u64, Hitos>> = std::sync::Mutex::new(std::collections::BTreeMap::new());
/// El último KickByNewSession (lo recibe la sesión vieja).
static KICK: std::sync::Mutex<Option<Instant>> = std::sync::Mutex::new(None);

/// Apunta un hito de la sesión `numero` (ahora).
pub fn hito(numero: u64, cual: impl FnOnce(&mut Hitos) -> &mut Option<Instant>) {
    if let Ok(mut h) = HITOS.lock() {
        // Sólo las últimas: el relevo no acumula para siempre.
        while h.len() > 8 { let k = *h.keys().next().unwrap(); h.remove(&k); }
        let e = h.entry(numero).or_default();
        let c = cual(e);
        if c.is_none() { *c = Some(Instant::now()) }
    }
}

pub fn hitos(numero: u64) -> Hitos { HITOS.lock().ok().and_then(|h| h.get(&numero).copied()).unwrap_or_default() }

/// ¿Ha dado audio ya la sesión `numero`? (La toma la más nueva que suena.)
pub fn dio_audio(numero: u64) -> bool { ACTIVA.load(Ordering::Relaxed) >= numero }

/// ¿Es `numero` la sesión que tiene el audio? (fuera del relevo, siempre).
pub fn es_activa(numero: u64) -> bool { numero == 0 || ACTIVA.load(Ordering::Relaxed) == numero }

pub fn kick() { if let Ok(mut k) = KICK.lock() { *k = Some(Instant::now()) } }

/// `relevo: kick +X, provisioned +X, configuración +X, ice +X, conectado +X, audio +X ms del /play; hueco N ms`.
fn resumen(numero: u64, hueco_ms: u64) -> String {
    let h = hitos(numero);
    let Some(play) = h.play else { return format!("relevo: hueco {hueco_ms} ms") };
    let ms = |t: Option<Instant>| t.filter(|t| *t >= play).map(|t| format!("+{}", t.duration_since(play).as_millis())).unwrap_or_else(|| "?".into());
    let kick = KICK.lock().ok().and_then(|k| *k);
    format!("relevo: kick {}, provisioned {}, configuración {}, ice {}, conectado {}, audio {} ms del /play; hueco {hueco_ms} ms",
        ms(kick), ms(h.provisioned), ms(h.configuracion), ms(h.ice), ms(h.conectado), ms(Some(Instant::now())))
}

/// `--play-anticipado=MS`: Play por la nube a MS ms del /play del
/// relevo, sin esperar al primer audio (el xccs tarda ~0,9 s en dar 200).
pub fn play_anticipado(numero: u64, t0: Instant, ms: u64) { play(numero, t0, Some(vec![ms]), "anticipado") }

static ACTIVA: AtomicU64 = AtomicU64::new(0);
static ULTIMO_AUDIO_MS: AtomicU64 = AtomicU64::new(0);

/// `Media/Play` por la nube tras cada relevo sin entrada: el
/// «remote» momentáneo del KickByNewSession pausa Spotify y no vuelve solo.
/// (autorización xccs, consola, esperas en ms desde que la sesión nueva da audio).
pub static PLAY_TRAS_RELEVO: std::sync::Mutex<Option<(String, String, Vec<u64>)>> = std::sync::Mutex::new(None);

/// esperas (ms) del Play al empezar, cuando la primera sesión da
/// audio: si Spotify estaba en pausa al hacer clic, no esperar al primer relevo.
pub static PLAY_AL_EMPEZAR: std::sync::Mutex<Option<Vec<u64>>> = std::sync::Mutex::new(None);

fn play_tras_relevo(numero: u64, t0: Instant) { play(numero, t0, None, "relevo") }

fn play(numero: u64, t0: Instant, otras_esperas: Option<Vec<u64>>, motivo: &'static str) {
    let Some((autorizacion, consola, esperas)) = PLAY_TRAS_RELEVO.lock().ok().and_then(|p| p.clone()) else { return };
    let esperas = otras_esperas.unwrap_or(esperas);
    // En otra tarea: nunca puede detener la sesión.
    tokio::spawn(async move {
        let inicio = Instant::now();
        for espera in esperas {
            tokio::time::sleep_until(tokio::time::Instant::from_std(inicio + std::time::Duration::from_millis(espera))).await;
            // no tras la parada.
            if PARADA.load(Ordering::Relaxed) { break }
            let r = crate::auth::orden_media(&autorizacion, &consola, "Play").await.unwrap_or_else(|e| format!("error: {e}"));
            registrar(t0, &format!("{}{motivo}: Play por la nube → {r} (+{} ms desde el {motivo})", etiqueta(numero), inicio.elapsed().as_millis()));
            // con el token rechazado no se insiste (el relevo siguiente trae otro).
            if rechazado(&r) { registrar(t0, &format!("{}{motivo}: xccs rechaza el token (401); sin más Play de éste", etiqueta(numero))); break }
        }
    });
}

/// al empezar el modo música, si en la consola no suena nada (al
/// acabar una sesión remota la consola cierra Spotify y luego sólo llega
/// silencio), se abre la app y, ~8 s después, Play, como se hacía a mano.
pub fn abrir_app_al_empezar(autorizacion: String, consola: String, producto: String, t0: Instant) {
    tokio::spawn(async move {
        match crate::auth::estado_consola(&autorizacion, &consola).await {
            Ok((_, repro)) if repro == "Playing" => { registrar(t0, "app: la consola ya está reproduciendo; no se toca"); return }
            Ok((foco, repro)) => registrar(t0, &format!("app: en la consola, «{}» y reproducción «{}»: se abre {producto}", if foco.is_empty() { "nada delante" } else { foco.as_str() }, if repro.is_empty() { "ninguna" } else { repro.as_str() })),
            Err(e) => { registrar(t0, &format!("app: no se pudo mirar la consola ({e}); no se toca")); return }
        }
        let r = crate::auth::abrir_app(&autorizacion, &consola, &producto).await.unwrap_or_else(|e| format!("error: {e}"));
        registrar(t0, &format!("app: abrir {producto} → {r}"));
        tokio::time::sleep(std::time::Duration::from_secs(8)).await;
        if PARADA.load(Ordering::Relaxed) { return }
        let r = crate::auth::orden_media(&autorizacion, &consola, "Play").await.unwrap_or_else(|e| format!("error: {e}"));
        registrar(t0, &format!("app: Play 8 s después → {r}"));
    });
}

/// Llega un paquete de audio de la sesión `numero`: ¿va a la reserva? Si es la
/// primera vez que esta sesión (más nueva) da audio, se queda la reserva, que se
/// reancla a su numeración, y se registra el hueco sin audio.
pub fn tomar_audio(numero: u64, reserva: &Option<ReservaCompartida>, t0: Instant) -> bool {
    let activa = ACTIVA.load(Ordering::Relaxed);
    if numero < activa { return false }
    let ahora_ms = t0.elapsed().as_millis() as u64;
    if numero > activa {
        ACTIVA.store(numero, Ordering::Relaxed);
        let ultimo = ULTIMO_AUDIO_MS.load(Ordering::Relaxed);
        if ultimo > 0 {
            if let Some(r) = reserva { if let Ok(mut r) = r.lock() { r.reanclar_tras(ahora_ms - ultimo) } }
            registrar(t0, &format!("{}relevo: esta sesión toma el audio; hueco de {} ms sin audio{}", etiqueta(numero), ahora_ms - ultimo, crate::volcado::marca()));
            registrar(t0, &format!("{}{}", etiqueta(numero), resumen(numero, ahora_ms - ultimo)));
            play_tras_relevo(numero, t0);
        } else if let Some(esperas) = PLAY_AL_EMPEZAR.lock().ok().and_then(|p| p.clone()) {
            play(numero, t0, Some(esperas), "inicio");
        }
    }
    ULTIMO_AUDIO_MS.store(ahora_ms, Ordering::Relaxed);
    true
}

/// lo que tarda el Play por la nube en hacer efecto (1,3-2,2 s, a
/// veces 2,9) es lo que queda de los cortes. En cuanto la reserva ve que lo
/// nuevo ha caído en la pausa de Spotify, Play en el acto y otro cada 700 ms
/// mientras siga (4 como mucho), además de los de por tiempo.
pub const REACCION_CADA_MS: u64 = 700;
pub const REACCION_TOPE: u32 = 4;

/// la parada del programa (la tarea de reacción termina) y un 401
/// de xccs (no se insiste con el mismo token: el relevo siguiente trae otro).
pub static PARADA: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static RECHAZADO: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn rechazado(respuesta: &str) -> bool { respuesta.starts_with("401") }

/// una reacción por ventana de relevo. Sólo
/// termina con música de verdad (`Volvio`) o al acabarse la ventana sin ella
/// (`Agotada`); un paquete con nivel entre dos tramos de pausa no la reinicia, así
/// que el tope de 4 vale para todo el relevo.
#[derive(Default)]
pub struct Reaccion { relevo: u64, desde: Option<Instant>, mandados: u32, ultimo: Option<Instant>, rendida: bool }

#[derive(Debug, PartialEq)]
pub enum Paso { Nada, Play(u32), Volvio { pausa_ms: u64, plays: u32 }, Agotada { pausa_ms: u64, plays: u32 } }

impl Reaccion {
    /// Cada vistazo a la reserva: el relevo en curso y en qué está lo nuevo.
    pub fn paso(&mut self, relevo: u64, estado: Pausa, ahora: Instant) -> Paso {
        if self.desde.is_some() && relevo != self.relevo { *self = Reaccion::default() }
        let fin = |r: &mut Reaccion, d: Instant| { let x = (ahora.duration_since(d).as_millis() as u64, r.mandados); *r = Reaccion::default(); x };
        match (estado, self.desde) {
            (Pausa::EnPausa, None) => { *self = Reaccion { relevo, desde: Some(ahora), mandados: 1, ultimo: Some(ahora), rendida: false }; Paso::Play(1) }
            (Pausa::EnPausa, Some(_)) if !self.rendida && self.mandados < REACCION_TOPE && self.ultimo.is_some_and(|u| ahora.duration_since(u).as_millis() as u64 >= REACCION_CADA_MS) => {
                self.mandados += 1;
                self.ultimo = Some(ahora);
                Paso::Play(self.mandados)
            }
            (Pausa::Musica, Some(d)) => { let (pausa_ms, plays) = fin(self, d); Paso::Volvio { pausa_ms, plays } }
            (Pausa::Fuera, Some(d)) => { let (pausa_ms, plays) = fin(self, d); Paso::Agotada { pausa_ms, plays } }
            _ => Paso::Nada,
        }
    }

    /// xccs dijo 401: no más Play en esta pausa.
    pub fn rendirse(&mut self) { self.rendida = true }
}

/// La tarea que vigila la reserva cada 20 ms y manda los Play (con `--tapar-relevo`
/// y Play tras relevo; `--sin-play-en-pausa` la quita). Termina con la parada.
pub fn reaccionar_a_la_pausa(reserva: ReservaCompartida, t0: Instant) {
    tokio::spawn(async move {
        let mut r = Reaccion::default();
        let mut cada = crate::red::intervalo(std::time::Duration::from_millis(20));
        while !PARADA.load(Ordering::Relaxed) {
            cada.tick().await;
            let Some((relevo, estado)) = reserva.lock().ok().and_then(|r| r.tapar).map(|t| (t.relevos, t.pausa())) else { continue };
            let numero = ACTIVA.load(Ordering::Relaxed);
            if RECHAZADO.swap(false, Ordering::Relaxed) && r.desde.is_some() {
                r.rendirse();
                registrar(t0, &format!("{}pausa: xccs rechaza el token (401); sin más Play hasta el relevo siguiente", etiqueta(numero)));
            }
            match r.paso(relevo, estado, Instant::now()) {
                Paso::Nada => {}
                Paso::Play(n) => {
                    let Some((autorizacion, consola, _)) = PLAY_TRAS_RELEVO.lock().ok().and_then(|p| p.clone()) else { continue };
                    let desde = r.desde.unwrap_or_else(Instant::now);
                    tokio::spawn(async move {
                        let salida = desde.elapsed().as_millis();
                        let res = crate::auth::orden_media(&autorizacion, &consola, "Play").await.unwrap_or_else(|e| format!("error: {e}"));
                        if rechazado(&res) { RECHAZADO.store(true, Ordering::Relaxed) }
                        registrar(t0, &format!("{}pausa: Play {n}/{REACCION_TOPE} por la nube a +{salida} ms de la pausa → {res} (+{} ms)", etiqueta(numero), desde.elapsed().as_millis()));
                    });
                }
                Paso::Volvio { pausa_ms, plays } => registrar(t0, &format!("{}pausa: volvió la música a +{pausa_ms} ms de verse la pausa ({plays} Play)", etiqueta(numero))),
                Paso::Agotada { pausa_ms, plays } => registrar(t0, &format!("{}pausa: SIN música a +{pausa_ms} ms de verse la pausa ({plays} Play): se acabó la ventana de 3 s del relevo", etiqueta(numero))),
            }
        }
    });
}

#[cfg(test)]
mod pruebas {
    use super::*;
    use std::time::Duration;

    #[test]
    fn reaccion_a_la_pausa() {
        use Pausa::*;
        let t = Instant::now();
        let ms = |m: u64| t + Duration::from_millis(m);
        let mut r = Reaccion::default();
        assert_eq!(r.paso(1, Esperando, ms(0)), Paso::Nada);
        assert_eq!(r.paso(1, EnPausa, ms(20)), Paso::Play(1));
        assert_eq!(r.paso(1, EnPausa, ms(700)), Paso::Nada);
        assert_eq!(r.paso(1, EnPausa, ms(720)), Paso::Play(2));
        // Una cabeza suelta (fase 1) no reinicia: ni Volvio ni otro Play(1).
        assert_eq!(r.paso(1, Esperando, ms(740)), Paso::Nada);
        assert_eq!(r.paso(1, EnPausa, ms(760)), Paso::Nada);
        assert_eq!(r.paso(1, EnPausa, ms(1420)), Paso::Play(3));
        assert_eq!(r.paso(1, EnPausa, ms(2120)), Paso::Play(4));
        assert_eq!(r.paso(1, EnPausa, ms(2820)), Paso::Nada, "tope de 4");
        assert_eq!(r.paso(1, Musica, ms(3020)), Paso::Volvio { pausa_ms: 3000, plays: 4 });
        assert_eq!(r.paso(1, Musica, ms(3040)), Paso::Nada);
        // El relevo siguiente: la ventana se acaba sin música.
        assert_eq!(r.paso(2, EnPausa, ms(60_000)), Paso::Play(1));
        assert_eq!(r.paso(2, Fuera, ms(63_000)), Paso::Agotada { pausa_ms: 3000, plays: 1 });
        assert_eq!(r.paso(2, Fuera, ms(63_020)), Paso::Nada);
        // Un 401: no se insiste.
        assert_eq!(r.paso(3, EnPausa, ms(120_000)), Paso::Play(1));
        r.rendirse();
        assert_eq!(r.paso(3, EnPausa, ms(121_000)), Paso::Nada);
        // Otro relevo con la reacción aún abierta: empieza de cero.
        assert_eq!(r.paso(4, EnPausa, ms(180_000)), Paso::Play(1));
        assert!(rechazado("401 Unauthorized") && !rechazado("200 OK"));
    }
}
