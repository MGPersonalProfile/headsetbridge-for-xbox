//! el HTTPS de todo el cliente (auth.rs, xhome.rs, xccs) detrás de
//! una sola interfaz: método, URL, cabeceras y cuerpo → estado y texto. Dos
//! motores: reqwest (features `schannel` o `rustls`) y WinHTTP de Windows
//! (feature `winhttp`: sin reqwest, hyper, url ni ICU; ~470 KiB menos medidos).
//! Con `winhttp` manda WinHTTP aunque haya otra.
//!
//! WinHTTP va síncrono dentro de `spawn_blocking` (son unas decenas de
//! peticiones por minuto). La sesión WinHTTP guarda las conexiones abiertas y
//! las reutiliza, que es lo que hace la «conexión caliente» de xccs; usa el
//! proxy del sistema.

use std::time::Duration;

type Resultado<T> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

#[derive(Debug, Clone, PartialEq)]
pub struct Respuesta {
    pub estado: u16,
    pub cuerpo: String,
}

impl Respuesta {
    pub fn bien(&self) -> bool { (200..300).contains(&self.estado) }
}

/// `https://host[:puerto]/ruta?consulta` → (host, puerto, ruta con la consulta).
pub fn partir_url(url: &str) -> Option<(String, u16, String)> {
    let resto = url.strip_prefix("https://")?;
    let (autoridad, ruta) = match resto.find('/') { Some(i) => (&resto[..i], &resto[i..]), None => (resto, "/") };
    let (host, puerto) = match autoridad.rsplit_once(':') {
        Some((h, p)) => (h, p.parse().ok()?),
        None => (autoridad, 443),
    };
    if host.is_empty() { return None }
    Some((host.to_owned(), puerto, ruta.to_owned()))
}

/// Las cabeceras como las quiere WinHTTP: «Nombre: valor\r\n» seguidas.
pub fn cabeceras_texto(cabeceras: &[(&str, &str)]) -> String {
    cabeceras.iter().map(|(k, v)| format!("{k}: {v}\r\n")).collect()
}

/// Un cliente; clonarlo comparte las conexiones abiertas.
#[derive(Clone)]
pub struct Http {
    #[cfg(not(all(windows, feature = "winhttp")))]
    cliente: reqwest::Client,
    #[cfg(all(windows, feature = "winhttp"))]
    sesion: std::sync::Arc<winhttp::Sesion>,
}

impl Http {
    /// `plazo`: lo máximo por petición. `persistente`: las conexiones ociosas
    /// se guardan 90 s y llevan keepalive TCP.
    pub fn nuevo(plazo: Duration, persistente: bool) -> Resultado<Self> {
        #[cfg(not(all(windows, feature = "winhttp")))]
        {
            let mut b = reqwest::Client::builder().timeout(plazo);
            if persistente { b = b.pool_idle_timeout(Duration::from_secs(90)).tcp_keepalive(Duration::from_secs(15)) }
            Ok(Self { cliente: b.build()? })
        }
        #[cfg(all(windows, feature = "winhttp"))]
        { Ok(Self { sesion: std::sync::Arc::new(winhttp::Sesion::abrir(plazo, persistente)?) }) }
    }

    pub async fn pedir(&self, metodo: &str, url: &str, cabeceras: &[(&str, &str)], cuerpo: Option<String>) -> Resultado<Respuesta> {
        #[cfg(not(all(windows, feature = "winhttp")))]
        {
            let m = reqwest::Method::from_bytes(metodo.as_bytes())?;
            let mut p = self.cliente.request(m, url);
            for (k, v) in cabeceras { p = p.header(*k, *v) }
            if let Some(c) = cuerpo { p = p.body(c) }
            let r = p.send().await?;
            let estado = r.status().as_u16();
            Ok(Respuesta { estado, cuerpo: r.text().await? })
        }
        #[cfg(all(windows, feature = "winhttp"))]
        {
            let (sesion, metodo, url, cabeceras) = (self.sesion.clone(), metodo.to_owned(), url.to_owned(), cabeceras_texto(cabeceras));
            tokio::task::spawn_blocking(move || sesion.pedir(&metodo, &url, &cabeceras, cuerpo.as_deref())).await?
        }
    }
}

#[cfg(all(windows, feature = "winhttp"))]
mod winhttp {
    use super::{partir_url, Respuesta, Resultado};
    use std::ffi::c_void;
    use std::time::Duration;
    use windows::core::{HSTRING, PCWSTR};
    use windows::Win32::Networking::WinHttp::*;

    /// Un HINTERNET que se cierra al soltarlo.
    struct Asa(*mut c_void);
    impl Drop for Asa { fn drop(&mut self) { unsafe { let _ = WinHttpCloseHandle(self.0); } } }

    fn asa(h: *mut c_void, que: &str) -> Resultado<Asa> {
        if h.is_null() { Err(format!("WinHTTP: {que}: {}", windows::core::Error::from_win32()).into()) } else { Ok(Asa(h)) }
    }

    pub struct Sesion(Asa);
    // El HINTERNET de sesión de WinHTTP se puede usar desde varios hilos.
    unsafe impl Send for Sesion {}
    unsafe impl Sync for Sesion {}

    impl Sesion {
        pub fn abrir(plazo: Duration, persistente: bool) -> Resultado<Self> {
            unsafe {
                let s = asa(WinHttpOpen(&HSTRING::from("xbox-solo-audio"), WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, PCWSTR::null(), PCWSTR::null(), 0), "WinHttpOpen")?;
                let ms = plazo.as_millis().min(i32::MAX as u128) as i32;
                WinHttpSetTimeouts(s.0, ms, ms, ms, ms)?;
                if persistente {
                    // Keepalive TCP: activado, a los 15 s y cada 15 s (tcp_keepalive de Winsock).
                    let ka: [u32; 3] = [1, 15_000, 15_000];
                    let bytes = std::slice::from_raw_parts(ka.as_ptr() as *const u8, std::mem::size_of_val(&ka));
                    let _ = WinHttpSetOption(Some(s.0), WINHTTP_OPTION_TCP_KEEPALIVE, Some(bytes));
                }
                Ok(Self(s))
            }
        }

        pub fn pedir(&self, metodo: &str, url: &str, cabeceras: &str, cuerpo: Option<&str>) -> Resultado<Respuesta> {
            let (host, puerto, ruta) = partir_url(url).ok_or_else(|| format!("URL no https: {url}"))?;
            unsafe {
                let conexion = asa(WinHttpConnect(self.0 .0, &HSTRING::from(host), puerto, 0), "WinHttpConnect")?;
                let peticion = asa(WinHttpOpenRequest(conexion.0, &HSTRING::from(metodo), &HSTRING::from(ruta), PCWSTR::null(), PCWSTR::null(), std::ptr::null(), WINHTTP_FLAG_SECURE), "WinHttpOpenRequest")?;
                let cabeceras: Vec<u16> = cabeceras.encode_utf16().collect();
                let cuerpo = cuerpo.unwrap_or("").as_bytes();
                let largo = cuerpo.len() as u32;
                WinHttpSendRequest(peticion.0, (!cabeceras.is_empty()).then_some(&cabeceras[..]), (largo > 0).then_some(cuerpo.as_ptr() as *const c_void), largo, largo, 0)?;
                WinHttpReceiveResponse(peticion.0, std::ptr::null_mut())?;
                let mut estado = 0u32;
                let mut tam = std::mem::size_of::<u32>() as u32;
                WinHttpQueryHeaders(peticion.0, WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER, PCWSTR::null(), Some(&mut estado as *mut u32 as *mut c_void), &mut tam, std::ptr::null_mut())?;
                let mut datos = Vec::new();
                loop {
                    let mut hay = 0u32;
                    WinHttpQueryDataAvailable(peticion.0, &mut hay)?;
                    if hay == 0 { break }
                    let inicio = datos.len();
                    datos.resize(inicio + hay as usize, 0);
                    let mut leidos = 0u32;
                    WinHttpReadData(peticion.0, datos[inicio..].as_mut_ptr() as *mut c_void, hay, &mut leidos)?;
                    datos.truncate(inicio + leidos as usize);
                    if leidos == 0 { break }
                }
                Ok(Respuesta { estado: estado as u16, cuerpo: String::from_utf8_lossy(&datos).into_owned() })
            }
        }
    }
}

#[cfg(test)]
mod pruebas {
    use super::*;

    #[test]
    fn urls_y_cabeceras() {
        assert_eq!(partir_url("https://xccs.xboxlive.com/commands"), Some(("xccs.xboxlive.com".into(), 443, "/commands".into())));
        assert_eq!(partir_url("https://xccs.xboxlive.com/lists/devices?queryCurrentDevice=false&x=1"),
            Some(("xccs.xboxlive.com".into(), 443, "/lists/devices?queryCurrentDevice=false&x=1".into())));
        assert_eq!(partir_url("https://xccs.xboxlive.com/"), Some(("xccs.xboxlive.com".into(), 443, "/".into())));
        assert_eq!(partir_url("https://weu.core.gssv-play-prod.xboxlive.com"), Some(("weu.core.gssv-play-prod.xboxlive.com".into(), 443, "/".into())));
        assert_eq!(partir_url("https://h:8443/a"), Some(("h".into(), 8443, "/a".into())));
        assert_eq!(partir_url("http://h/a"), None);
        assert_eq!(cabeceras_texto(&[("A", "1"), ("B-C", "x y")]), "A: 1\r\nB-C: x y\r\n");
        assert!(Respuesta { estado: 204, cuerpo: String::new() }.bien() && !Respuesta { estado: 401, cuerpo: String::new() }.bien());
    }
}
