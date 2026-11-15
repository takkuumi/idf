#!/usr/bin/env python3
"""Run the actual TCP fair-schedule helper on the host."""

from __future__ import annotations

import subprocess
import re
import tempfile
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
SOURCE = (ROOT / "src/modbus/tcp_server.rs").read_text()
CONFIG = (ROOT / "src/config.rs").read_text()
MAX_CONNECTIONS = int(re.search(
    r"pub const MAX_CONNECTIONS: usize = (\d+);", CONFIG
).group(1))


def item(text: str, marker: str) -> str:
    start = text.index(marker)
    opening = text.index("{", start)
    depth = 1
    end = opening + 1
    while depth:
        depth += (text[end] == "{") - (text[end] == "}")
        end += 1
    return text[start:end]


helper = item(SOURCE, "fn fair_client_schedule(")
request_limit = re.search(r"^const MAX_REQUESTS_PER_TICK:.*;", SOURCE, re.M).group(0)
admission_limit = re.search(r"^const FIRST_REQUEST_TIMEOUT:.*;", SOURCE, re.M).group(0)
program = f"""
#![allow(dead_code)]
mod cfg {{ pub const MAX_CONNECTIONS: usize = {MAX_CONNECTIONS}; pub const IDLE_TIMEOUT_MS:u64=120000; pub const TX_TIMEOUT_MS:u64=5000; pub const PARTIAL_FRAME_TIMEOUT_MS:u64=30000; }}
{request_limit}
{admission_limit}
{helper}

#[test]
fn rotates_distinct_ports_before_revisiting() {{
    let clients = [(1, 0), (2, 1), (3, 2), (4, 3)];
    let (order, len) = fair_client_schedule(&clients, 0, &[None; 4]);
    assert_eq!(&order[..len], &[0, 1, 2, 3]);
}}

#[test]
fn rotates_clients_on_same_port_by_last_id() {{
    let clients = [(10, 0), (11, 0), (20, 1)];
    let (order, len) = fair_client_schedule(&clients, 0, &[Some(10), None, None, None]);
    assert_eq!(&order[..len], &[1, 0, 2]);
}}

#[test]
fn empty_and_missing_ids_are_bounded() {{
    let (_, len) = fair_client_schedule(&[], 0, &[None; 4]);
    assert_eq!(len, 0);
    let (order, len) = fair_client_schedule(&[(7, 3)], 99, &[Some(99); 4]);
    assert_eq!(&order[..len], &[0]);
}}
"""

# Exercise the production socket state machine/scheduler with a deterministic
# backend. Only business response construction/keepalive are host substitutes.
program += "\n" + "\n".join(item(SOURCE, marker) for marker in (
    "struct Client {", "impl Client {", "fn client_timed_out_at(",
    "struct TcpServerState {", "fn poll_clients(", "fn client_is_admission_reclaimable(",
    "fn client_peer_closed(", "fn transient_accept_error(", "fn listener_has_fatal_error("))
program += r"""
use std::io::{ErrorKind,Read,Write};
use std::net::{TcpStream,TcpListener,SocketAddr};
use std::sync::atomic::{AtomicU32,Ordering};
use std::sync::Mutex;
use std::time::{Duration,Instant};
const MAX_ADU_SIZE:usize=260;
const MBAP_PREFIX_LEN:usize=6;
const MAX_MBAP_LENGTH:usize=254;
const MAX_REQUESTS_PER_POLL:usize=4;
const MAX_TCP_WORK_US:u64=2500;
static CONN_COUNT:AtomicU32=AtomicU32::new(0);
static CALLS:Mutex<Vec<u16>>=Mutex::new(Vec::new());
extern crate self as log;
#[macro_export] macro_rules! debug { ($($arg:tt)*) => {let _=format_args!($($arg)*);}; }
#[macro_export] macro_rules! warn { ($($arg:tt)*) => {let _=format_args!($($arg)*);}; }
mod esp_idf_sys {
    // ESP-IDF/newlib target errno ABI; it differs from host Darwin/Linux.
    pub const EAGAIN:u32=11; pub const ENOMEM:u32=12; pub const ENFILE:u32=23;
    pub const EMFILE:u32=24; pub const ECONNRESET:u32=104; pub const ENOBUFS:u32=105;
    pub const ECONNABORTED:u32=113; pub const ETIMEDOUT:u32=116; pub const EHOSTUNREACH:u32=118;
}
struct BusBackend;
fn configure_keepalive(_: &TcpStream)->std::io::Result<()> {Ok(())}
fn build_response(request:&[u8],_:&BusBackend,response:&mut[u8;MAX_ADU_SIZE])->Option<usize> {
    CALLS.lock().unwrap().push(u16::from_be_bytes([request[0],request[1]]));
    if request[7]==0x7e {std::thread::sleep(Duration::from_millis(5));}
    response[..request.len()].copy_from_slice(request);
    Some(request.len())
}
fn state(slots:&[usize])->(TcpServerState,Vec<TcpStream>) {
    CALLS.lock().unwrap().clear();CONN_COUNT.store(slots.len() as u32,Ordering::Relaxed);
    let ports=[502,503,504,5002];let mut clients=Vec::new();let mut peers=Vec::new();
    for (index,&slot) in slots.iter().enumerate() {
        let listener=TcpListener::bind("127.0.0.1:0").unwrap();
        let peer=TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (stream,addr)=listener.accept().unwrap();
        let id=index as u32+1;
        let mut client=Client::new(id,ports[slot],slot,addr,stream).unwrap();
        let frame=[0,id as u8,0,0,0,6,1,5,2,0,0,0];
        client.rx[..frame.len()].copy_from_slice(&frame);client.rx_len=frame.len();
        clients.push(client);peers.push(peer);
    }
    let now=Instant::now();
    (TcpServerState {listeners:Vec::new(),clients,active_ports:ports,next_port_check:now,
      last_served_client:[None;4],port_cursor:0,listener_cursor:0,listener_rebind_due:None,
      next_reject_log:now,rejected_connections:0,conn_rate_window:now,conn_rate_count:0,
      conn_established_total:0,conn_closed_total:0},peers)
}
#[test]
fn timeout_boundaries_use_a_single_consistent_sample() {
    let start=Instant::now();
    assert!(!client_timed_out_at(start,None,None,start,false));
    assert!(!client_timed_out_at(start+Duration::from_millis(cfg::TX_TIMEOUT_MS-1),None,None,start,true));
    assert!(client_timed_out_at(start+Duration::from_millis(cfg::PARTIAL_FRAME_TIMEOUT_MS),Some(start),None,start,false));
    assert!(client_timed_out_at(start+Duration::from_millis(cfg::TX_TIMEOUT_MS),None,Some(start),start,true));
    assert!(client_timed_out_at(start+Duration::from_millis(cfg::IDLE_TIMEOUT_MS),None,None,start,false));
}
#[test]
fn actual_scheduler_admits_one_request_per_tick_and_rotates_ports() {
    let (mut s,_peers)=state(&[0,1,2,3]);
    poll_clients(&mut s,Instant::now());assert_eq!(*CALLS.lock().unwrap(),[1]);
    assert_eq!(s.port_cursor,1);
    poll_clients(&mut s,Instant::now());assert_eq!(*CALLS.lock().unwrap(),[1,2]);
    poll_clients(&mut s,Instant::now());assert_eq!(*CALLS.lock().unwrap(),[1,2,3]);
    poll_clients(&mut s,Instant::now());assert_eq!(*CALLS.lock().unwrap(),[1,2,3,4]);
}
#[test]
fn actual_scheduler_time_budget_preserves_next_unserved_port() {
    let (mut s,_peers)=state(&[0,1,2,3]);s.clients[0].rx[7]=0x7e;
    poll_clients(&mut s,Instant::now());assert_eq!(*CALLS.lock().unwrap(),[1]);
    assert_eq!(s.port_cursor,1);
    poll_clients(&mut s,Instant::now());assert_eq!(*CALLS.lock().unwrap(),[1,2]);
}
#[test]
fn actual_scheduler_rotates_same_port_clients_without_starving_another() {
    let (mut s,_peers)=state(&[0,0,1]);
    poll_clients(&mut s,Instant::now());assert_eq!(*CALLS.lock().unwrap(),[1]);
    poll_clients(&mut s,Instant::now());assert_eq!(*CALLS.lock().unwrap(),[1,3]);
    poll_clients(&mut s,Instant::now());assert_eq!(*CALLS.lock().unwrap(),[1,3,2]);
    let (mut s,_peers)=state(&[0,0]);
    poll_clients(&mut s,Instant::now());assert_eq!(*CALLS.lock().unwrap(),[1]);
}
#[test]
fn actual_scheduler_services_all_configured_connections_fairly() {
    let slots:Vec<usize>=(0..cfg::MAX_CONNECTIONS).map(|index|index%4).collect();
    let (mut s,_peers)=state(&slots);
    for _ in 0..cfg::MAX_CONNECTIONS {poll_clients(&mut s,Instant::now());}
    let mut calls=CALLS.lock().unwrap().clone();calls.sort();
    assert_eq!(calls,(1..=cfg::MAX_CONNECTIONS as u16).collect::<Vec<_>>());
}
#[test]
fn admission_deadline_expires_only_unverified_sessions() {
    let now=Instant::now();
    let (mut client,_peer)=test_client(now,Duration::ZERO,0,false,0);
    client.first_request_deadline=Some(now+FIRST_REQUEST_TIMEOUT);
    assert!(!client_is_admission_reclaimable(&client,now+FIRST_REQUEST_TIMEOUT-Duration::from_millis(1)));
    assert!(client_is_admission_reclaimable(&client,now+FIRST_REQUEST_TIMEOUT));
    client.rx_len=1;client.last_activity=now+FIRST_REQUEST_TIMEOUT;
    assert!(client_is_admission_reclaimable(&client,now+FIRST_REQUEST_TIMEOUT));
    client.first_request_deadline=None;
    assert!(!client_is_admission_reclaimable(&client,now+Duration::from_secs(60)));
}
#[test]
fn valid_first_request_enables_persistent_session_reuse() {
    let (mut s,_peers)=state(&[0]);
    assert!(s.clients[0].first_request_deadline.is_some());
    poll_clients(&mut s,Instant::now());
    assert!(s.clients[0].first_request_deadline.is_none());
    assert_eq!(s.clients[0].id,1);
    for id in 2..=20u8 {
        let frame=[0,id,0,0,0,6,1,5,2,0,0,0];
        s.clients[0].rx[..12].copy_from_slice(&frame);s.clients[0].rx_len=12;
        poll_clients(&mut s,Instant::now());
        assert_eq!(s.clients.len(),1);assert_eq!(s.clients[0].id,1);
    }
    assert_eq!(CALLS.lock().unwrap().len(),20);
}
#[test]
fn unverified_deadline_releases_slots_without_displacing_verified_idle_client() {
    let now=Instant::now();
    let (mut s,_peers)=state(&[0]);
    s.clients[0].rx_len=0;s.clients[0].first_request_deadline=Some(now-Duration::from_millis(1));
    poll_clients(&mut s,now);
    assert!(s.clients.is_empty());assert!(CALLS.lock().unwrap().is_empty());
    let (mut s,_peers)=state(&[0]);
    poll_clients(&mut s,Instant::now());
    s.clients[0].last_activity=Instant::now()-Duration::from_secs(20);
    poll_clients(&mut s,Instant::now());
    assert_eq!(s.clients.len(),1,"confirmed idle PLC connection must be retained");
}
#[test]
fn admission_reclaim_detects_closed_peer_immediately() {
    let now=Instant::now();
    let (mut client, peer)=test_client(now, Duration::from_secs(0), 0, false, 0);
    assert!(!client_peer_closed(&client));
    drop(peer);
    let mut closed=false;
    for _ in 0..100 { if client_peer_closed(&client) { closed=true; break; } std::thread::sleep(Duration::from_millis(1)); }
    assert!(closed);
    client.rx_len=1;
    assert!(!client_peer_closed(&client),"half-close cannot discard pending request");
    client.rx_len=0;client.tx_len=1;
    assert!(!client_peer_closed(&client),"half-close cannot discard pending response");
}
#[test]
fn transient_accept_pressure_does_not_mark_listener_failed() {
    assert!(transient_accept_error(&std::io::Error::from_raw_os_error(105)));
    assert!(transient_accept_error(&std::io::Error::from_raw_os_error(113)));
    assert!(transient_accept_error(&std::io::Error::from_raw_os_error(23)));
    assert!(transient_accept_error(&std::io::Error::from_raw_os_error(12)));
    assert!(!transient_accept_error(&std::io::Error::from_raw_os_error(22)));
    let listener=TcpListener::bind("127.0.0.1:0").unwrap();
    assert!(!listener_has_fatal_error(&listener));
}
fn test_client(now:Instant, idle:Duration, rx_len:usize, partial:bool, tx_len:usize)->(Client,TcpStream) {
    let listener=TcpListener::bind("127.0.0.1:0").unwrap();
    let peer=TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    let (stream,_)=listener.accept().unwrap(); stream.set_nonblocking(true).unwrap();
    let client=Client { id:1, listener_port:502, initial_slot:0, peer:"127.0.0.1:1234".parse().unwrap(),
        stream, rx:[0;MAX_ADU_SIZE], rx_len, tx:[0;MAX_ADU_SIZE], tx_len, tx_sent:0,
        last_activity:now-idle, partial_frame_started:if partial {Some(now)} else {None}, tx_started:None, first_request_deadline:None };
    (client,peer)
}
#[test]
fn actual_scheduler_checks_timeouts_and_flushes_even_when_deadline_expired() {
    let (mut s,mut peers)=state(&[0,1]);
    s.clients[0].tx[0]=0xaa;s.clients[0].tx_len=1;
    s.clients[1].last_activity=Instant::now()-Duration::from_millis(cfg::IDLE_TIMEOUT_MS+1);
    poll_clients(&mut s,Instant::now()-Duration::from_millis(3));
    assert!(CALLS.lock().unwrap().is_empty());assert_eq!(s.clients.len(),1);
    peers[0].set_read_timeout(Some(Duration::from_millis(100))).unwrap();
    let mut byte=[0];peers[0].read_exact(&mut byte).unwrap();assert_eq!(byte,[0xaa]);
}
#[test]
fn actual_scheduler_does_not_orphan_connections_after_port_rebind() {
    let (mut s,_peers)=state(&[3]);s.active_ports[3]=15002;
    poll_clients(&mut s,Instant::now());assert_eq!(*CALLS.lock().unwrap(),[1]);
}
#[test]
fn actual_client_never_executes_buffered_work_after_deadline() {
    let (mut s,_peers)=state(&[0]);let mut allowance=1;
    assert!(s.clients[0].poll(&BusBackend,&mut allowance,Instant::now()-Duration::from_millis(1)));
    assert_eq!(allowance,1);assert_eq!(s.clients[0].rx_len,12);
    assert!(CALLS.lock().unwrap().is_empty());
}
"""

# Keep admission constants identical to production, not hand-copied test values.
for constant, placeholder in (
    ("MAX_TCP_WORK_US", "const MAX_TCP_WORK_US:u64=2500;"),
    ("MAX_REQUESTS_PER_POLL", "const MAX_REQUESTS_PER_POLL:usize=4;"),
):
    actual = re.search(r"^const " + constant + r":.*;", SOURCE, re.M).group(0)
    program = program.replace(placeholder, actual)

with tempfile.TemporaryDirectory(prefix="gateway-tcp-scheduler-") as directory:
    path = Path(directory)
    (path / "tests.rs").write_text(program)
    subprocess.run(
        ["rustc", "+stable", "--edition=2024", "--test", str(path / "tests.rs"), "-o", str(path / "tests")],
        check=True,
    )
    subprocess.run([str(path / "tests"), "--test-threads=1"], check=True, timeout=30)
