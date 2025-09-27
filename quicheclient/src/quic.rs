/* src/quic.rs */
use mio::net::UdpSocket;
use mio::{Events, Interest, Poll, Token};
use quiche::h3::{Config as H3Config, Connection as H3Conn, Header, NameValue};
use quiche::{Config, ConnectionId};
use std::net::SocketAddr;
use std::time::{Duration, Instant};

pub const CLIENT: Token = Token(0);
pub const MAX_DGRAM: usize = 1350;

/* ────────────────────────────── Public API ────────────────────────────── */

/// Single HTTP/3 GET of `path` from `base`, with progress logging.
/// Matches the behavior of your current `main.rs`.
pub fn single_get(
    base: &str, path: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    // ---- Target ------------------------------------------------------------
    let url = url::Url::parse(base)?;
    let authority = url.host_str().unwrap().as_bytes();
    let path_bytes = path.as_bytes();

    // ---- UDP socket --------------------------------------------------------
    let peer_addr = resolve_loopback_4433()?;
    let bind_addr: SocketAddr = match peer_addr {
        SocketAddr::V4(_) => "0.0.0.0:0".parse().unwrap(),
        SocketAddr::V6(_) => "[::]:0".parse().unwrap(),
    };

    let mut sock = UdpSocket::bind(bind_addr)?;
    let local_addr = sock.local_addr()?;
    let mut poll = Poll::new()?;
    poll.registry()
        .register(&mut sock, CLIENT, Interest::READABLE)?;
    let mut events = Events::with_capacity(1024);

    // ---- QUIC config + connect --------------------------------------------
    let mut cfg = build_quiche_config()?;
    let (mut conn, mut out) =
        connect(&url, local_addr, peer_addr, &mut cfg, &mut sock)?;

    let mut h3: Option<H3Conn> = None;
    let h3_cfg = H3Config::new()?;

    // request state
    let mut sent_sid: Option<u64> = None;
    let mut received: u64 = 0;
    let mut content_len: Option<u64> = None;
    let mut last_prog = Instant::now();
    let start = Instant::now();

    // ---- Event loop --------------------------------------------------------
    loop {
        // Wait for socket or timeout
        poll.poll(&mut events, conn.timeout())?;
        if events.is_empty() {
            // timer fired
            conn.on_timeout();
        }

        // ---- Read incoming UDP and feed to quiche -------------------------
        'read: loop {
            let mut buf = [0u8; 65535];

            match sock.recv_from(&mut buf) {
                Ok((len, from)) => {
                    let info = quiche::RecvInfo {
                        to: local_addr,
                        from,
                    };
                    match conn.recv(&mut buf[..len], info) {
                        Ok(_) => {},
                        Err(quiche::Error::Done) => break 'read,
                        Err(e) => {
                            eprintln!("[err] quiche recv: {e:?}");
                            break 'read;
                        },
                    }
                },
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    break 'read
                },
                Err(e) => return Err(format!("udp recv error: {e:?}").into()),
            }
        }

        // ---- On establishment, print ALPN and send GET --------------------
        if conn.is_established() && h3.is_none() {
            let alpn = String::from_utf8_lossy(conn.application_proto());
            eprintln!("[init] ALPN={}", alpn);

            let mut c = H3Conn::with_transport(&mut conn, &h3_cfg)?;

            let req = vec![
                Header::new(b":method", b"GET"),
                Header::new(b":scheme", b"https"),
                Header::new(b":authority", authority),
                Header::new(b":path", path_bytes),
                Header::new(b"user-agent", b"h3-sponza-client"),
            ];
            let sid = c.send_request(&mut conn, &req, true)?;
            sent_sid = Some(sid);
            eprintln!("[h3] sent GET {} on sid={}", path, sid);

            h3 = Some(c);
        }

        // ---- Drive HTTP/3 --------------------------------------------------
        if let Some(c) = &mut h3 {
            loop {
                match c.poll(&mut conn) {
                    Ok((sid, ev)) => match ev {
                        quiche::h3::Event::Headers { list, .. } => {
                            let mut status = None;
                            for h in list {
                                if h.name() == b":status" {
                                    status = Some(
                                        String::from_utf8_lossy(h.value())
                                            .into_owned(),
                                    );
                                }
                                if h.name() == b"content-length" {
                                    if let Ok(s) = std::str::from_utf8(h.value())
                                    {
                                        content_len = s.parse::<u64>().ok();
                                    }
                                }
                            }
                            if let Some(s) = status {
                                eprintln!(
                                    "[h3] sid={} :status {} content-length={:?}",
                                    sid, s, content_len
                                );
                            }
                        },
                        quiche::h3::Event::Data => {
                            let mut buf = [0u8; 64 * 1024];
                            loop {
                                match c.recv_body(&mut conn, sid, &mut buf) {
                                    Ok(read) => {
                                        if read == 0 {
                                            break;
                                        }
                                        received += read as u64;

                                        if last_prog.elapsed()
                                            >= Duration::from_millis(250)
                                        {
                                            last_prog = Instant::now();
                                            if let Some(cl) = content_len {
                                                let pct = (received as f64
                                                    / cl as f64)
                                                    * 100.0;
                                                eprintln!(
                                                    "[h3] sid={} progress: {:.1}%  ({:.1}/{:.1} MiB)",
                                                    sid, pct, mib(received), mib(cl)
                                                );
                                            } else {
                                                eprintln!(
                                                    "[h3] sid={} progress: {:.1} MiB",
                                                    sid, mib(received)
                                                );
                                            }
                                        }
                                    },
                                    Err(quiche::h3::Error::Done) => break,
                                    Err(e) => {
                                        return Err(format!(
                                            "recv_body error: {e:?}"
                                        )
                                        .into())
                                    },
                                }
                            }
                        },
                        quiche::h3::Event::Finished => {
                            let dur = start.elapsed().as_secs_f64();
                            eprintln!(
                                "[h3] sid={} FIN total={:.1} MiB time={:.2}s avg={:.1} MiB/s",
                                sid,
                                mib(received),
                                dur,
                                mib_per_s(received, dur)
                            );
                            // graceful close
                            conn.close(true, 0x00, b"done").ok();
                        },
                        _ => {},
                    },
                    Err(quiche::h3::Error::Done) => break,
                    Err(e) => return Err(format!("h3 poll error: {e:?}").into()),
                }
            }
        }

        // ---- Generate and send UDP packets --------------------------------
        loop {
            let (n, send_info) = match conn.send(&mut out) {
                Ok(v) => v,
                Err(quiche::Error::Done) => break,
                Err(e) => return Err(format!("send failed: {e:?}").into()),
            };
            send_to(&sock, &out[..n], send_info.to)?;
        }

        // ---- Exit conditions ----------------------------------------------
        if conn.is_closed() {
            if let Some(sid) = sent_sid {
                eprintln!(
                    "[end] connection closed; stream sid={} done; bytes {:.1} MiB",
                    sid,
                    mib(received)
                );
            } else {
                eprintln!("[end] connection closed before request");
            }
            break;
        }
    }

    Ok(())
}

/* ──────────────── Scaffolding for future experiments (not used yet) ─────────────── */

/// Planned request descriptor for experiments. Owns `path` to avoid lifetimes.

#[allow(dead_code)]
pub struct Req {
    pub path: Vec<u8>,
    pub urgency: u8,
    pub incremental: bool,
    pub tag: &'static str,
}

/// Per-stream stats for summaries.
#[allow(dead_code)]
#[derive(Clone)]
pub struct Stat {
    pub sid: u64,
    pub tag: &'static str,
    pub urgency: u8,
    pub incremental: bool,
    pub bytes: u64,
    pub t_sent: Instant,
    pub t_first: Option<Instant>,
    pub t_fin: Option<Instant>,
}

pub struct MultiResult {
    pub rows: Vec<Stat>,
}

#[allow(dead_code)]
impl Stat {
    pub fn new(
        sid: u64, tag: &'static str, urgency: u8, incremental: bool,
    ) -> Self {
        Self {
            sid,
            tag,
            urgency,
            incremental,
            bytes: 0,
            t_sent: Instant::now(),
            t_first: None,
            t_fin: None,
        }
    }
}

/* ────────────────────────────── Internals ────────────────────────────── */

fn build_quiche_config() -> Result<Config, Box<dyn std::error::Error>> {
    let mut cfg = Config::new(quiche::PROTOCOL_VERSION)?;
    cfg.set_application_protos(quiche::h3::APPLICATION_PROTOCOL)?;
    cfg.set_max_idle_timeout(120_000);
    cfg.set_initial_max_data(200_000_000);
    cfg.set_initial_max_stream_data_bidi_local(32_000_000);
    cfg.set_initial_max_stream_data_bidi_remote(32_000_000);
    cfg.set_initial_max_stream_data_uni(2_000_000);
    cfg.set_initial_max_streams_bidi(64);
    cfg.set_initial_max_streams_uni(4);
    cfg.set_max_recv_udp_payload_size(MAX_DGRAM);
    cfg.set_max_send_udp_payload_size(MAX_DGRAM);
    cfg.verify_peer(false); // local/dev only
    Ok(cfg)
}

fn connect(
    url: &url::Url, local_addr: SocketAddr, peer_addr: SocketAddr,
    cfg: &mut Config, sock: &mut UdpSocket,
) -> Result<(quiche::Connection, [u8; MAX_DGRAM]), Box<dyn std::error::Error>> {
    let cid_bytes = rand_bytes(16);
    let scid = ConnectionId::from_ref(&cid_bytes);

    let mut conn =
        quiche::connect(url.domain(), &scid, local_addr, peer_addr, cfg)?;

    // initial flight
    let mut out = [0u8; MAX_DGRAM];
    let (written, send_info) = conn.send(&mut out).expect("initial send failed");
    send_to(sock, &out[..written], send_info.to)?;
    Ok((conn, out))
}

fn send_to(
    sock: &UdpSocket, data: &[u8], to: SocketAddr,
) -> Result<(), Box<dyn std::error::Error>> {
    loop {
        match sock.send_to(data, to) {
            Ok(_) => return Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => continue,
            Err(e) => return Err(format!("udp send_to error: {e:?}").into()),
        }
    }
}

fn resolve_loopback_4433() -> Result<SocketAddr, Box<dyn std::error::Error>> {
    Ok("127.0.0.1:4433".parse().unwrap())
}

fn rand_bytes(len: usize) -> Vec<u8> {
    use rand::{RngCore, SeedableRng};
    let mut v = vec![0u8; len];
    let mut rng = rand::rngs::StdRng::seed_from_u64(0x_0123_4567_89ab_cdef);
    rng.fill_bytes(&mut v);
    v
}

fn mib(b: u64) -> f64 {
    b as f64 / (1024.0 * 1024.0)
}

fn mib_per_s(b: u64, s: f64) -> f64 {
    if s > 0.0 {
        mib(b) / s
    } else {
        0.0
    }
}

/* Drop this into src/quic.rs, replacing the current stub. No other files need changes. */
pub fn multi_get_with_priorities(
    base: &str,
    reqs: &[Req],
    prio_updates: Option<Vec<(usize, quiche::h3::Priority, Duration)>>, // (req index, new prio, delay)
) -> Result<MultiResult, Box<dyn std::error::Error>> {
    use std::collections::BTreeMap;

    let url = url::Url::parse(base)?;
    let authority = url.host_str().unwrap().as_bytes();

    // Socket
    let peer_addr = resolve_loopback_4433()?;
    let bind_addr: SocketAddr = match peer_addr {
        SocketAddr::V4(_) => "0.0.0.0:0".parse().unwrap(),
        SocketAddr::V6(_) => "[::]:0".parse().unwrap(),
    };

    let mut sock = mio::net::UdpSocket::bind(bind_addr)?;
    let local_addr = sock.local_addr()?;
    let mut poll = mio::Poll::new()?;
    poll.registry()
        .register(&mut sock, CLIENT, mio::Interest::READABLE)?;
    let mut events = mio::Events::with_capacity(1024);

    // QUIC
    let mut cfg = build_quiche_config()?;
    let (mut conn, mut out) =
        connect(&url, local_addr, peer_addr, &mut cfg, &mut sock)?;

    // HTTP/3
    let mut h3: Option<H3Conn> = None;
    let h3_cfg = H3Config::new()?;

    // Stats
    let mut stats: BTreeMap<u64, Stat> = BTreeMap::new(); // sid -> Stat
    let mut idx_to_sid: BTreeMap<usize, u64> = BTreeMap::new();

    // Build a one-shot schedule for priority updates so each fires only once.
    let t0 = Instant::now();
    let (sched, mut fired): (Vec<(usize, usize, Instant)>, Vec<bool>) =
        if let Some(upds) = &prio_updates {
            let s: Vec<(usize, usize, Instant)> = upds
                .iter()
                .enumerate()
                // (k = index into `prio_updates`, req_idx, due_time)
                .map(|(k, (req_idx, _p, delay))| (k, *req_idx, t0 + *delay))
                .collect();
            let f = vec![false; upds.len()];
            (s, f)
        } else {
            (Vec::new(), Vec::new())
        };

    // Event loop
    loop {
        // Wait
        poll.poll(&mut events, conn.timeout())?;
        if events.is_empty() {
            conn.on_timeout();
        }

        // RX UDP
        'read: loop {
            let mut buf = [0u8; 65535];
            match sock.recv_from(&mut buf) {
                Ok((len, from)) => {
                    let info = quiche::RecvInfo { to: local_addr, from };
                    match conn.recv(&mut buf[..len], info) {
                        Ok(_) => {}
                        Err(quiche::Error::Done) => break 'read,
                        Err(e) => {
                            return Err(format!("quiche recv error: {e:?}").into())
                        }
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break 'read,
                Err(e) => return Err(format!("udp recv error: {e:?}").into()),
            }
        }

        // Create H3 and send requests
        if conn.is_established() && h3.is_none() {
            let alpn = String::from_utf8_lossy(conn.application_proto());
            eprintln!("[exp] ALPN={}", alpn);

            let mut c = H3Conn::with_transport(&mut conn, &h3_cfg)?;

            for (i, r) in reqs.iter().enumerate() {
                let prio_hdr = if r.incremental {
                    format!("u={},i", r.urgency)
                } else {
                    format!("u={}", r.urgency)
                };

                let req = vec![
                    Header::new(b":method", b"GET"),
                    Header::new(b":scheme", b"https"),
                    Header::new(b":authority", authority),
                    Header::new(b":path", r.path.as_slice()),
                    Header::new(b"priority", prio_hdr.as_bytes()),
                    Header::new(b"user-agent", b"h3-exp-client"),
                ];
                let sid = c.send_request(&mut conn, &req, true)?;
                let _ = conn.stream_priority(sid, r.urgency, r.incremental);

                let st = Stat {
                    sid,
                    tag: r.tag,
                    urgency: r.urgency,
                    incremental: r.incremental,
                    bytes: 0,
                    t_sent: Instant::now(),
                    t_first: None,
                    t_fin: None,
                };
                stats.insert(sid, st);
                idx_to_sid.insert(i, sid);

                eprintln!(
                    "[exp] sent sid={} tag={} priority=(u={}, i={})",
                    sid,
                    r.tag,
                    r.urgency,
                    if r.incremental { 1 } else { 0 }
                );
            }

            h3 = Some(c);
        }

        // One-shot PRIORITY_UPDATEs (multiple)
        if let (Some(upds), Some(c)) = (prio_updates.as_ref(), h3.as_mut()) {
            let now = Instant::now();
            for (k, req_idx, due) in sched.iter().copied() {
                if !fired[k] && now >= due {
                    if let Some(&sid) = idx_to_sid.get(&req_idx) {
                        let prio = &upds[k].1;
                        let _ = c.send_priority_update_for_request(&mut conn, sid, prio);
                        eprintln!("[exp] PRIORITY_UPDATE sid={} -> {:?}", sid, prio);
                    }
                    fired[k] = true; // fire once
                }
            }
        }

        // Drive H3
        if let Some(c) = &mut h3 {
            loop {
                match c.poll(&mut conn) {
                    Ok((sid, ev)) => match ev {
                        quiche::h3::Event::Headers { list, .. } => {
                            let mut status = None;
                            for h in list {
                                if h.name() == b":status" {
                                    status = Some(
                                        String::from_utf8_lossy(h.value())
                                            .into_owned(),
                                    );
                                }
                            }
                            if let Some(s) = status {
                                eprintln!("[exp] sid={} :status {}", sid, s);
                                if let Some(st) = stats.get_mut(&sid) {
                                    if st.t_first.is_none() {
                                        st.t_first = Some(Instant::now());
                                    }
                                }
                            }
                        }
                        quiche::h3::Event::Data => {
                            let mut buf = [0u8; 64 * 1024];
                            loop {
                                match c.recv_body(&mut conn, sid, &mut buf) {
                                    Ok(read) => {
                                        if read == 0 {
                                            break;
                                        }
                                        if let Some(st) = stats.get_mut(&sid) {
                                            st.bytes += read as u64;
                                            if st.t_first.is_none() {
                                                st.t_first = Some(Instant::now());
                                            }
                                        }
                                    }
                                    Err(quiche::h3::Error::Done) => break,
                                    Err(e) => {
                                        return Err(format!("recv_body error: {e:?}").into())
                                    }
                                }
                            }
                        }
                        quiche::h3::Event::Finished => {
                            if let Some(st) = stats.get_mut(&sid) {
                                st.t_fin = Some(Instant::now());
                                eprintln!(
                                    "[exp] sid={} FIN tag={} bytes={:.1} MiB",
                                    sid, st.tag, mib(st.bytes)
                                );
                            }
                        }
                        _ => {}
                    },
                    Err(quiche::h3::Error::Done) => break,
                    Err(e) => return Err(format!("h3 poll error: {e:?}").into()),
                }
            }
        }

        // Flush
        loop {
            match conn.send(&mut out) {
                Ok((n, send_info)) => {
                    send_to(&sock, &out[..n], send_info.to)?;
                }
                Err(quiche::Error::Done) => break,
                Err(e) => return Err(format!("send failed: {e:?}").into()),
            }
        }

        // Exit when done
        if !stats.is_empty() && stats.values().all(|s| s.t_fin.is_some()) {
            break;
        }
        if conn.is_closed() {
            break;
        }
    }

    // Summary
    let mut rows: Vec<Stat> = stats.into_values().collect();
    rows.sort_by_key(|r| r.sid);
    println!("\n[exp] SUMMARY: ...");
    Ok(MultiResult { rows })
}
