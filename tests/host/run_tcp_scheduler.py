#!/usr/bin/env python3
"""Run the actual TCP fair-schedule helper on the host."""

from __future__ import annotations

import subprocess
import re
import tempfile
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
SOURCE = (ROOT / "src/modbus/tcp_server.rs").read_text()


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
program = f"""
#![allow(dead_code)]
mod cfg {{ pub const MAX_CONNECTIONS: usize = 8; pub const IDLE_TIMEOUT_MS:u64=120000; pub const TX_TIMEOUT_MS:u64=5000; pub const PARTIAL_FRAME_TIMEOUT_MS:u64=30000; }}
{request_limit}
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
    "struct Client {", "impl Client {", "struct TcpServerState {", "fn poll_clients("))
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
fn actual_scheduler_admits_two_distinct_ports_then_remaining_ports() {
    let (mut s,_peers)=state(&[0,1,2,3]);
    poll_clients(&mut s,Instant::now());assert_eq!(*CALLS.lock().unwrap(),[1,2]);
    assert_eq!(s.port_cursor,2);
    poll_clients(&mut s,Instant::now());assert_eq!(*CALLS.lock().unwrap(),[1,2,3,4]);
}
#[test]
fn actual_scheduler_time_budget_preserves_next_unserved_port() {
    let (mut s,_peers)=state(&[0,1,2,3]);s.clients[0].rx[7]=0x7e;
    poll_clients(&mut s,Instant::now());assert_eq!(*CALLS.lock().unwrap(),[1]);
    assert_eq!(s.port_cursor,1);
    poll_clients(&mut s,Instant::now());assert_eq!(*CALLS.lock().unwrap(),[1,2,3]);
}
#[test]
fn actual_scheduler_borrows_spare_slot_for_same_port_without_starving_another() {
    let (mut s,_peers)=state(&[0,0,1]);
    poll_clients(&mut s,Instant::now());assert_eq!(*CALLS.lock().unwrap(),[1,3]);
    poll_clients(&mut s,Instant::now());assert_eq!(*CALLS.lock().unwrap(),[1,3,2]);
    let (mut s,_peers)=state(&[0,0]);
    poll_clients(&mut s,Instant::now());assert_eq!(*CALLS.lock().unwrap(),[1,2]);
}
#[test]
fn actual_scheduler_services_all_eight_connections_in_four_ticks() {
    let (mut s,_peers)=state(&[0,0,1,1,2,2,3,3]);
    for _ in 0..4 {poll_clients(&mut s,Instant::now());}
    let mut calls=CALLS.lock().unwrap().clone();calls.sort();assert_eq!(calls,[1,2,3,4,5,6,7,8]);
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
