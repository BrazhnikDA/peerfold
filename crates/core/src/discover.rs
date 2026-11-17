use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use if_addrs::IfAddr;
use mdns_sd::{ServiceDaemon, ServiceEvent, ServiceInfo};

use crate::model::SERVICE_TYPE;
use crate::state::Inner;

pub fn spawn(inner: Arc<Inner>) -> JoinHandle<()> {
    thread::spawn(move || {
        let Ok(mdns) = ServiceDaemon::new() else {
            inner.log("Не удалось запустить поиск компьютеров");
            return;
        };
        let Some(ip) = lan_ip() else {
            inner.log("Нет IPv4-адреса в локальной сети");
            return;
        };
        let port = inner.port.load(Ordering::Relaxed);
        let host = format!("{}.local.", inner.device_id);
        let take = 8.min(inner.device_id.len());
        let instance = format!("pf{}", &inner.device_id[..take]);
        let id = inner.device_id.clone();
        let properties = [("id", id.as_str())];
        match ServiceInfo::new(SERVICE_TYPE, &instance, &host, ip.to_string(), port, &properties[..]) {
            Ok(info) => {
                if let Err(err) = mdns.register(info) {
                    inner.log(format!("Не удалось объявить себя в сети: {err}"));
                }
            }
            Err(err) => inner.log(format!("Не удалось объявить себя в сети: {err}")),
        }
        let Ok(events) = mdns.browse(SERVICE_TYPE) else {
            inner.log("Поиск компьютеров не запустился");
            return;
        };
        let mut last_retry = Instant::now();
        loop {
            if inner.shutdown.load(Ordering::Relaxed) {
                break;
            }
            match events.recv_timeout(Duration::from_secs(1)) {
                Ok(ServiceEvent::ServiceResolved(info)) => note_service(&inner, info.as_ref()),
                Ok(_) => {}
                Err(mdns_sd::RecvTimeoutError::Disconnected) => break,
                Err(mdns_sd::RecvTimeoutError::Timeout) => {}
            }
            if last_retry.elapsed() >= Duration::from_secs(3) {
                last_retry = Instant::now();
                retry(&inner);
            }
        }
        let _ = mdns.shutdown();
    })
}

fn note_service(inner: &Arc<Inner>, info: &mdns_sd::ResolvedService) {
    let Some(device_id) = info.get_property_val_str("id") else {
        return;
    };
    if device_id == inner.device_id {
        return;
    }
    let Some(ip) = info.get_addresses_v4().into_iter().next() else {
        return;
    };
    let addr = SocketAddr::new(IpAddr::V4(ip), info.get_port());
    inner.note_addr(device_id, device_id, addr);
    if inner.device_id.as_str() < device_id && !inner.online(device_id) {
        crate::session::dial(Arc::clone(inner), device_id.to_string(), addr);
    }
}

fn retry(inner: &Arc<Inner>) {
    let peers: Vec<(String, SocketAddr)> = inner
        .live
        .lock()
        .expect("live")
        .iter()
        .filter(|(id, peer)| inner.device_id.as_str() < id.as_str() && !peer.online)
        .filter_map(|(id, peer)| peer.addr.map(|addr| (id.clone(), addr)))
        .collect();
    for (id, addr) in peers {
        crate::session::dial(Arc::clone(inner), id, addr);
    }
}

fn lan_ip() -> Option<std::net::Ipv4Addr> {
    let mut fallback = None;
    for iface in if_addrs::get_if_addrs().ok()? {
        let IfAddr::V4(ref addr) = iface.addr else {
            continue;
        };
        if iface.is_loopback() || addr.ip.is_link_local() {
            continue;
        }
        let octets = addr.ip.octets();
        let private = octets[0] == 10
            || (octets[0] == 192 && octets[1] == 168)
            || (octets[0] == 172 && (16..32).contains(&octets[1]));
        if private {
            return Some(addr.ip);
        }
        fallback = Some(addr.ip);
    }
    fallback
}
