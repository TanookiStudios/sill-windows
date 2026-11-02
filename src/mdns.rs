//! Bonjour: `<PC name>._sill._tcp.local` on this PC's port, with the TXT record a Sill device
//! reads (Pairing.swift's HomeDoorTXT and RecognitionTag): `p=1` (TLS, pairing required) and `r`,
//! the recognition tag that tells a paired device this is its PC whatever the name. Windows 10 and
//! 11 answer for it themselves (DnsServiceRegister), so nothing else needs installing.

#[cfg(windows)]
pub struct Advertisement {
    request: Box<windows::Win32::NetworkManagement::Dns::DNS_SERVICE_REGISTER_REQUEST>,
    /// Set by `withdrawn` once Windows has taken the advertisement down.
    gone: Box<std::sync::atomic::AtomicBool>,
}

#[cfg(windows)]
impl Drop for Advertisement {
    /// Withdraws it, so devices stop listing this PC as soon as the host quits. Withdrawal finishes
    /// in the background: wait for it (a second at most) before the request and instance go.
    fn drop(&mut self) {
        use windows::Win32::NetworkManagement::Dns::*;
        unsafe extern "system" fn withdrawn(_: u32, context: *const core::ffi::c_void, instance: *const DNS_SERVICE_INSTANCE) {
            if !instance.is_null() {
                DnsServiceFreeInstance(instance);
            }
            (*(context as *const std::sync::atomic::AtomicBool)).store(true, std::sync::atomic::Ordering::SeqCst);
        }
        unsafe {
            self.request.pRegisterCompletionCallback = Some(withdrawn);
            self.request.pQueryContext = &*self.gone as *const _ as *mut _;
            if DnsServiceDeRegister(&*self.request, None) == 9506 {
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
                while !self.gone.load(std::sync::atomic::Ordering::SeqCst) && std::time::Instant::now() < deadline {
                    std::thread::sleep(std::time::Duration::from_millis(20));
                }
            }
            if self.gone.load(std::sync::atomic::Ordering::SeqCst) {
                DnsServiceFreeInstance(self.request.pServiceInstance);
            } // else: Windows may still read it; leaking it at exit is the safe choice
        }
    }
}

/// This PC's network name ("Maddies-PC"), as Windows announces it on the local network.
#[cfg(windows)]
pub fn host_name() -> Option<String> {
    use windows::Win32::System::SystemInformation::{ComputerNameDnsHostname, GetComputerNameExW};
    let mut buf = [0u16; 256];
    let mut len = buf.len() as u32;
    unsafe { GetComputerNameExW(ComputerNameDnsHostname, windows::core::PWSTR(buf.as_mut_ptr()), &mut len).ok()? };
    Some(String::from_utf16_lossy(&buf[..len as usize])).filter(|n| !n.is_empty())
}

#[cfg(not(windows))]
pub fn host_name() -> Option<String> {
    None
}

#[cfg(windows)]
pub fn advertise(name: &str, port: u16, txt: &[(&str, String)], ipv4: Option<std::net::Ipv4Addr>) -> Result<Advertisement, String> {
    use windows::core::{HSTRING, PCWSTR};
    use windows::Win32::NetworkManagement::Dns::*;

    unsafe extern "system" fn done(status: u32, _: *const core::ffi::c_void, instance: *const DNS_SERVICE_INSTANCE) {
        if status == 0 {
            println!("Advertised on the network: Sill on iPad and iPhone can see this PC now.");
        } else {
            eprintln!("Couldn't advertise this PC on the network (error {status}); devices won't list it.");
        }
        if !instance.is_null() {
            DnsServiceFreeInstance(instance);
        }
    }

    let host = host_name().unwrap_or_else(|| "windows-pc".into());
    let service = HSTRING::from(format!("{name}._sill._tcp.local"));
    let host = HSTRING::from(format!("{host}.local"));
    let keys: Vec<HSTRING> = txt.iter().map(|(k, _)| HSTRING::from(*k)).collect();
    let values: Vec<HSTRING> = txt.iter().map(|(_, v)| HSTRING::from(v.as_str())).collect();
    let kp: Vec<PCWSTR> = keys.iter().map(|k| PCWSTR(k.as_ptr())).collect();
    let vp: Vec<PCWSTR> = values.iter().map(|v| PCWSTR(v.as_ptr())).collect();
    unsafe {
        // The address too, so the name answers even where Windows doesn't announce its own (in
        // network byte order: the octets as they sit in memory).
        let ip4 = ipv4.map(|a| u32::from_ne_bytes(a.octets()));
        // The instance copies every string, so the HSTRINGs may go once it is made.
        let instance = DnsServiceConstructInstance(
            &service, &host, ip4.as_ref().map(|a| a as *const u32), None, port, 0, 0, kp.len() as u32, kp.as_ptr(), vp.as_ptr(),
        );
        if instance.is_null() {
            return Err("Windows couldn't build the network advertisement".into());
        }
        let request = Box::new(DNS_SERVICE_REGISTER_REQUEST {
            Version: DNS_QUERY_REQUEST_VERSION1.0,
            InterfaceIndex: 0,
            pServiceInstance: instance,
            pRegisterCompletionCallback: Some(done),
            pQueryContext: std::ptr::null_mut(),
            hCredentials: Default::default(),
            unicastEnabled: false.into(),
        });
        let status = DnsServiceRegister(&*request, None);
        if status != 9506 {
            // 9506, DNS_REQUEST_PENDING: accepted, finishing in the background (`done` says how).
            DnsServiceFreeInstance(instance);
            return Err(format!("Windows refused the network advertisement (error {status})"));
        }
        Ok(Advertisement { request, gone: Box::new(false.into()) })
    }
}

// shortcut: outside Windows nothing is advertised (development only); tests dial the port directly.
#[cfg(not(windows))]
pub struct Advertisement;

#[cfg(not(windows))]
pub fn advertise(name: &str, port: u16, txt: &[(&str, String)], _ipv4: Option<std::net::Ipv4Addr>) -> Result<Advertisement, String> {
    let record: Vec<String> = txt.iter().map(|(k, v)| format!("{k}={v}")).collect();
    println!("(Not on Windows: not advertising \"{name}\" on port {port} with {}.)", record.join(" "));
    Ok(Advertisement)
}
