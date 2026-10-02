//! Señalización HTTP de xHome: lo mismo que el prototipo en Node.

use std::sync::atomic::{AtomicU8, Ordering};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

type Resultado<T> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

const DEVICE_INFO: &str = r#"{"appInfo":{"env":{"clientAppId":"Microsoft.GamingApp","clientAppType":"native","clientAppVersion":"2203.1001.4.0","clientSdkVersion":"8.5.2","httpEnvironment":"prod","sdkInstallId":""}},"dev":{"hw":{"make":"Microsoft","model":"Surface Pro","sdktype":"native"},"os":{"name":"Windows 11","ver":"22631.2715","platform":"desktop"},"displayInfo":{"dimensions":{"widthInPixels":1920,"heightInPixels":1080},"pixelDensity":{"dpiX":1,"dpiY":1}}}}"#;

/// `--ajuste-play=clave=valor` (diagnóstico, repetible): sustituye un campo de
/// `settings` en /play; el valor se lee como JSON y, si no lo es, como texto.
pub static AJUSTES_PLAY: std::sync::Mutex<Vec<(String, String)>> = std::sync::Mutex::new(Vec::new());

/// `--reenganchar`: 0 = cerrar la sesión abandonada
/// antes de /play (por defecto); 1 = no cerrarla ni hacer DELETE al cortarse,
/// para que el /play siguiente vuelva a ella; 2 = además, pedirla por su id en
/// `clientSessionId`.
pub static REENGANCHAR: AtomicU8 = AtomicU8::new(0);

/// Las sesiones abiertas por este proceso (más de una durante un relevo).
static VIVAS: std::sync::Mutex<std::collections::BTreeSet<String>> = std::sync::Mutex::new(std::collections::BTreeSet::new());

/// ¿Hay que dejar abierta la sesión al cortarse (para re-engancharse)?
pub fn reenganchar() -> bool { REENGANCHAR.load(Ordering::Relaxed) > 0 }

/// sondeos HTTP rápidos. `/state` y los GET de
/// espera cada 50 ms en vez de 250, y el reintento por 204 a 50 ms en vez de
/// 200. Cada uno es tiempo del hueco del relevo.
pub static RAPIDO: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
fn cada_ms(normal: u64) -> Duration { Duration::from_millis(if RAPIDO.load(Ordering::Relaxed) { 50 } else { normal }) }

/// Lo que entrega auth.rs. Nunca se imprime.
pub struct Credenciales {
    pub host: String,
    pub region: String,
    pub gs_token: String,
    pub lpt: String,
    /// La consola elegida por auth.rs cuando no se pasa `--consola`.
    pub consola: Option<String>,
    /// La consola estaba en reposo y auth.rs la encendió (Power/WakeUp).
    pub encendida: bool,
    /// Autorización para xccs (`XBL3.0 x=uhs;token`): Media/Play tras un relevo.
    pub xccs: Option<String>,
}

/// Clonable para pedir cosas en paralelo sin bloquear la sesión (p. ej. `/ice` a mitad).
#[derive(Clone)]
pub struct XHome {
    http: crate::http::Http,
    base: String,
    token: String,
    lpt: String,
    pub sesion: Option<String>,
}

impl XHome {
    pub fn nuevo(c: Credenciales) -> Resultado<Self> {
        Ok(Self {
            // Plazo por petición: una respuesta que no llega no puede colgar el arranque ni la parada.
            http: crate::http::Http::nuevo(Duration::from_secs(15), false)?,
            base: format!("https://{}", c.host),
            token: c.gs_token,
            lpt: c.lpt,
            sesion: None,
        })
    }

    async fn pedir(&self, metodo: &str, ruta: &str, cuerpo: Option<Value>, device_info: bool) -> Resultado<Value> {
        // 204 = «todavía no»: se repite a los 200 ms (Greenlight: 750) hasta 30 s;
        // cada ms cuenta en el hueco del relevo.
        let fin = Instant::now() + Duration::from_secs(30);
        loop {
            let autorizacion = format!("Bearer {}", self.token);
            let mut cabeceras = vec![("Content-Type", "application/json"), ("Authorization", autorizacion.as_str())];
            if device_info { cabeceras.push(("X-MS-Device-Info", DEVICE_INFO)) }
            let r = self.http.pedir(metodo, &format!("{}{}", self.base, ruta), &cabeceras, cuerpo.as_ref().map(|c| c.to_string())).await?;
            let (estado, texto) = (r.estado, r.cuerpo);
            if estado == 204 && metodo == "GET" {
                if Instant::now() > fin { return Err(format!("{metodo} {}: sin respuesta de la consola", sin_ids(ruta)).into()) }
                tokio::time::sleep(cada_ms(200)).await;
                continue;
            }
            if !(200..300).contains(&estado) {
                return Err(format!("{metodo} {} → {estado}: {}", sin_ids(ruta), texto.chars().take(300).collect::<String>()).into());
            }
            return Ok(serde_json::from_str(&texto).unwrap_or(Value::Null));
        }
    }

    fn ruta(&self, sufijo: &str) -> String {
        format!("/v5/sessions/home/{}{}", self.sesion.as_deref().unwrap_or(""), sufijo)
    }

    #[cfg(feature = "webrtc")]
    pub async fn arrancar(&mut self, consola: &str) -> Resultado<()> {
        self.arrancar_con(consola, json!({ "nanoVersion": "V3;WebrtcTransport.dll", "enableTextToSpeech": false, "highContrast": 0,
            "locale": "en-US", "useIceConnection": false, "timezoneOffsetMinutes": 120, "sdkType": "web", "osName": "windows" })).await
    }

    /// Ruta Nano V3 nativa (sin WebRTC): los ajustes de greenlight-native
    /// (`packages/xcloud/src/index.ts`, `_createSession`).
    pub async fn arrancar_nativo(&mut self, consola: &str) -> Resultado<()> {
        self.arrancar_con(consola, json!({ "nanoVersion": "V3", "enableTextToSpeech": false, "highContrast": 0,
            "locale": "en-US", "useIceConnection": true, "timezoneOffsetMinutes": 120, "sdkType": "native", "osName": "unknown", "magnifier": false })).await
    }

    async fn arrancar_con(&mut self, consola: &str, ajustes: Value) -> Resultado<()> {
        // Sin `--reenganchar`, la sesión que dejó abierta un arranque anterior
        // se cierra; con él, se vuelve a ella (y con `=id`, se le pide por su id).
        let mut cliente = String::new();
        match REENGANCHAR.load(Ordering::Relaxed) {
            0 => { self.cerrar_abandonada().await; }
            2 => { cliente = en_disco(|| std::fs::read_to_string(abierta())).await.map(|s| s.trim().to_owned()).unwrap_or_default(); }
            _ => {}
        }
        let mut cuerpo = json!({
            "titleId": "", "systemUpdateGroup": "", "clientSessionId": cliente,
            "settings": ajustes,
            "serverId": consola, "fallbackRegionNames": [],
        });
        for (clave, valor) in AJUSTES_PLAY.lock().map(|a| a.clone()).unwrap_or_default() {
            cuerpo["settings"][clave] = serde_json::from_str(&valor).unwrap_or(Value::String(valor));
        }
        let r = self.pedir("POST", "/v5/sessions/home/play", Some(cuerpo), true).await?;
        let ruta = r["sessionPath"].as_str().ok_or("la consola no devolvió sessionPath")?;
        self.sesion = ruta.split('/').nth(3).map(str::to_owned);
        // Apuntada en disco hasta su DELETE: si el proceso muere sin cerrarla
        // (pánico con panic=abort, matado), el arranque siguiente la cierra.
        // Una sesión sin DELETE deja la consola en «remoto» sin cliente y
        // bloquea /play.
        if let Some(id) = &self.sesion { if let Ok(mut v) = VIVAS.lock() { v.insert(id.clone()); } }
        if let Some(id) = self.sesion.clone() {
            let _ = en_disco(move || { std::fs::create_dir_all(abierta().parent().unwrap_or(std::path::Path::new(".")))?; std::fs::write(abierta(), id) }).await;
        }
        Ok(())
    }

    /// Sondea /state hasta Provisioned; en ReadyToConnect manda el token de /connect.
    pub async fn esperar_consola(&self, al_estado: impl FnMut(&str)) -> Resultado<()> {
        self.esperar_consola_cada(cada_ms(250), al_estado).await
    }

    pub async fn esperar_consola_cada(&self, cada: Duration, mut al_estado: impl FnMut(&str)) -> Resultado<()> {
        let inicio = Instant::now();
        let fin = inicio + Duration::from_secs(90);
        let mut anterior = String::new();
        // en un re-enganche, las primeras consultas (~250-330 ms tras
        // el /play) aún dan el Provisioned del transporte anterior. Sólo vale el
        // que llega tras otro estado, o pasados 3 s.
        let mut otro = !reenganchar();
        while Instant::now() < fin {
            let r = self.pedir("GET", &self.ruta("/state"), None, false).await?;
            let estado = r["state"].as_str().unwrap_or("").to_owned();
            if estado != anterior { al_estado(&estado); anterior = estado.clone() }
            match estado.as_str() {
                "Provisioned" if otro || inicio.elapsed() > Duration::from_secs(3) => return Ok(()),
                "Provisioned" => {}
                "Failed" => return Err(format!("la consola falló: {}", r["errorDetails"]).into()),
                "ReadyToConnect" => { otro = true; self.pedir("POST", &self.ruta("/connect"), Some(json!({ "userToken": self.lpt })), false).await?; }
                _ => otro = true,
            }
            tokio::time::sleep(cada).await;
        }
        Err("la consola no estuvo lista a tiempo".into())
    }

    pub async fn intercambiar_sdp(&self, sdp: &str) -> Resultado<String> {
        self.pedir("POST", &self.ruta("/sdp"), Some(json!({
            "messageType": "offer", "sdp": sdp,
            "configuration": {
                "chatConfiguration": { "bytesPerSample": 2, "expectedClipDurationMs": 20, "format": { "codec": "opus", "container": "webm" }, "numChannels": 1, "sampleFrequencyHz": 24000 },
                "chat": { "minVersion": 1, "maxVersion": 1 }, "control": { "minVersion": 1, "maxVersion": 3 },
                "input": { "minVersion": 1, "maxVersion": 8 }, "message": { "minVersion": 1, "maxVersion": 1 },
            },
        })), false).await?;
        let r = self.pedir("GET", &self.ruta("/sdp"), None, false).await?;
        // Con la consola ocupada o tras una sesión muerta llega null y el motivo en errorDetails.
        let intercambio = r["exchangeResponse"].as_str().ok_or_else(|| format!("la consola no respondió a la oferta SDP: {}", r["errorDetails"]))?;
        let respuesta: Value = serde_json::from_str(intercambio)?;
        Ok(respuesta["sdp"].as_str().ok_or("la consola rechazó la oferta SDP")?.to_owned())
    }

    /// Devuelve las líneas de candidato de la consola, sin el prefijo `a=`.
    pub async fn intercambiar_ice(&self, locales: &[String]) -> Resultado<Vec<String>> {
        let candidatos: Vec<Value> = locales.iter().map(|c| json!({ "candidate": c, "sdpMLineIndex": 0, "sdpMid": "0" })).collect();
        self.pedir("POST", &self.ruta("/ice"), Some(json!({ "messageType": "iceCandidate", "candidate": candidatos })), false).await?;
        let r = self.pedir("GET", &self.ruta("/ice"), None, false).await?;
        let remotos: Vec<Value> = serde_json::from_str(r["exchangeResponse"].as_str().unwrap_or("[]"))?;
        Ok(remotos.iter().filter_map(|c| c["candidate"].as_str())
            .map(|c| c.trim_start_matches("a=").to_owned())
            .filter(|c| !c.is_empty() && c != "end-of-candidates").collect())
    }

    /// Ruta nativa: las rutas v4 de greenlight-native (con ellas conecta
    /// el prototipo en Node).
    fn ruta_v4(&self, sufijo: &str) -> String {
        format!("/v4/sessions/home/{}{}", self.sesion.as_deref().unwrap_or(""), sufijo)
    }

    /// GET hasta que llegue `campo` (o un error de la consola), como `_isExchangeReady`.
    async fn esperar_campo(&self, ruta: &str, campo: &str) -> Resultado<Value> {
        self.esperar_campo_cada(ruta, campo, cada_ms(250)).await
    }

    async fn esperar_campo_cada(&self, ruta: &str, campo: &str, cada: Duration) -> Resultado<Value> {
        let fin = Instant::now() + Duration::from_secs(30);
        loop {
            let r = self.pedir("GET", ruta, None, false).await?;
            if !r["errorDetails"].is_null() && !r["errorDetails"]["code"].is_null() {
                return Err(format!("GET {}: {}", sin_ids(ruta), r["errorDetails"]).into());
            }
            if !r[campo].is_null() { return Ok(r) }
            if Instant::now() > fin { return Err(format!("GET {}: sin {campo}", sin_ids(ruta)).into()) }
            tokio::time::sleep(cada).await;
        }
    }

    /// Ruta nativa: `/configuration` → la respuesta entera (`serverDetails.srtp.key`, etc.).
    pub async fn configuracion_nativa(&self) -> Resultado<Value> {
        self.esperar_campo(&self.ruta_v4("/configuration"), "serverDetails").await
    }

    /// un solo GET a una ruta v4 de la sesión (`/configuration`,
    /// `/ice`), para sondearla desde fuera con su propio ritmo y criterio.
    pub async fn get_v4(&self, sufijo: &str) -> Resultado<Value> {
        self.pedir("GET", &self.ruta_v4(sufijo), None, false).await
    }

    /// Ruta nativa: manda el JSON de candidatos del cliente y devuelve el de la
    /// consola (`Candidates`, `Username`, `Password`).
    pub async fn intercambiar_ice_nativo(&self, candidatos: &Value) -> Resultado<Value> {
        self.pedir("POST", &self.ruta_v4("/ice"), Some(json!({ "candidates": candidatos.to_string() })), false).await?;
        let r = self.esperar_campo(&self.ruta_v4("/ice"), "candidates").await?;
        Ok(serde_json::from_str(r["candidates"].as_str().ok_or("candidates no es texto")?)?)
    }

    pub async fn ice_post(&self, candidatos: &Value) -> Resultado<()> {
        self.pedir("POST", &self.ruta_v4("/ice"), Some(json!({ "candidates": candidatos.to_string() })), false).await.map(|_| ())
    }

    pub async fn keepalive(&self) -> Resultado<()> {
        self.pedir("POST", &self.ruta("/keepalive"), Some(json!({})), false).await.map(|_| ())
    }

    pub async fn parar(&mut self) -> Resultado<()> {
        if self.sesion.is_none() { return Ok(()) }
        let ruta = self.ruta("");
        let id = self.sesion.take().unwrap_or_default();
        if let Ok(mut v) = VIVAS.lock() { v.remove(&id); }
        let r = self.pedir("DELETE", &ruta, None, false).await.map(|_| ());
        // Sólo si el fichero sigue siendo de ésta (en el relevo, ya apunta a la siguiente).
        if r.is_ok() { let _ = en_disco(move || if std::fs::read_to_string(abierta())?.trim() == id { std::fs::remove_file(abierta()) } else { Ok(()) }).await; }
        r
    }

    /// Cierra la sesión que dejó abierta un arranque anterior que murió sin
    /// DELETE. Devuelve si había una. Se llama antes de cada /play.
    pub async fn cerrar_abandonada(&self) -> bool {
        let Ok(id) = en_disco(|| std::fs::read_to_string(abierta())).await else { return false };
        let id = id.trim();
        if id.is_empty() || id.contains('/') { let _ = en_disco(|| std::fs::remove_file(abierta())).await; return false }
        // Una sesión viva de este mismo proceso (el relevo) no está abandonada.
        if VIVAS.lock().is_ok_and(|v| v.contains(id)) { return false }
        let r = self.pedir("DELETE", &format!("/v5/sessions/home/{id}"), None, false).await;
        // Cerrada, o ya no existe (4xx): no hay que volver a intentarlo.
        if r.is_ok() || r.as_ref().err().is_some_and(|e| e.to_string().contains("→ 4")) { let _ = en_disco(|| std::fs::remove_file(abierta())).await; }
        true
    }
}

/// El fichero de la sesión abierta se toca fuera del hilo de tokio (revisión
/// 19 §2.2): en un relevo, la sesión que sigue sonando comparte ese hilo.
async fn en_disco<T: Send + 'static>(f: impl FnOnce() -> std::io::Result<T> + Send + 'static) -> std::io::Result<T> {
    tokio::task::spawn_blocking(f).await.unwrap_or_else(|e| Err(std::io::Error::other(e)))
}

/// %LOCALAPPDATA%\XboxSoloAudio\sesion-abierta: el id de la sesión sin cerrar.
fn abierta() -> std::path::PathBuf {
    std::path::PathBuf::from(std::env::var("LOCALAPPDATA").unwrap_or_default()).join("XboxSoloAudio").join("sesion-abierta")
}

/// Huella corta de un secreto o un id para el registro (8 hex de SHA-1): deja
/// ver si dos valores coinciden sin escribirlos.
pub fn huella(valor: &str) -> String {
    use sha1::Digest;
    sha1::Sha1::digest(valor.as_bytes()).iter().take(4).map(|b| format!("{b:02x}")).collect()
}

/// Las rutas llevan el id de sesión: no se registra.
fn sin_ids(ruta: &str) -> String {
    ruta.split('/').map(|p| if p.len() >= 32 { "<id>" } else { p }).collect::<Vec<_>>().join("/")
}
