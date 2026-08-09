//! End-to-end encrypted RTP workload for comparing the `SharedBytes` backends.
//!
//! This benchmark models an SFU forwarding encoded RTP data. Setup and DTLS
//! handshakes happen before timing starts. Each timed operation then:
//!
//! 1. encrypts an RTP packet on an upstream connection;
//! 2. decrypts and parses it on the SFU-side connection;
//! 3. retains a clone of the received payload for each downstream connection; and
//! 4. encrypts each forwarded packet for its destination and consumes the
//!    resulting datagram.
//!
//! Run the atomic configuration:
//!
//! ```text
//! cargo bench --bench shared_bytes --no-default-features --features aws-lc-rs
//! ```
//!
//! Run the non-atomic configuration:
//!
//! ```text
//! cargo bench --bench shared_bytes --no-default-features --features aws-lc-rs,single-threaded
//! ```
//!
//! Compare the same fan-out and payload-size rows from the two runs. The
//! benchmark deliberately includes SRTP and the normal Rtc input/output path;
//! it is not intended to isolate reference-count instructions. The ingress
//! fan-out group also retains all clones until the operation completes. That
//! models queued downstream work and avoids measuring an artificial
//! clone-then-immediately-drop loop. It uses mimalloc so allocator behavior is
//! held constant across the two runs.
//!
//! Fixed-iteration Callgrind profiles are selected with
//! `SHARED_BYTES_VALGRIND=shared-ingress`, `vec-ingress`, `shared-relay`, or
//! `vec-relay`. They pause after setup so `callgrind_control` can enable
//! instrumentation only for the measured operations.

use std::hint::black_box;
use std::io::Write;
use std::time::{Duration, Instant};

use criterion::{BenchmarkId, Criterion, Throughput};
use mimalloc::MiMalloc;
use str0m::format::Codec;
use str0m::media::{MediaKind, Pt};
use str0m::rtp::{RtpWrite, Ssrc};
use str0m::{Event, Input, Output, Rtc, SharedBytes};

#[path = "../tests/common.rs"]
mod common;

use common::{Peer, TestRtc, connect_l_r_with_rtc, init_crypto_default, progress};

const PAYLOAD_SIZE: usize = 1200;
const RTP_TIME_STEP: u32 = 960;

#[global_allocator]
static GLOBAL: MiMalloc = MiMalloc;

fn connect_media_peers() -> (TestRtc, TestRtc) {
    // The shared test helper enables raw packet capture, which adds a Vec copy
    // for every packet. A normal SFU does not need that diagnostic path, so
    // leave it disabled here while retaining the real DTLS/SRTP negotiation.
    let mut left = Rtc::builder().set_rtp_mode(true);
    if let Some(crypto) = Peer::Left.crypto_provider() {
        left = left.set_crypto_provider(crypto);
    }

    let mut right = Rtc::builder().set_rtp_mode(true);
    if let Some(crypto) = Peer::Right.crypto_provider() {
        right = right.set_crypto_provider(crypto);
    }

    let now = Instant::now();
    connect_l_r_with_rtc(left.build(now), right.build(now))
}

struct Connection {
    sender: TestRtc,
    receiver: TestRtc,
    payload_type: Pt,
    ssrc: Ssrc,
    sequence_number: u64,
    rtp_time: u32,
}

impl Connection {
    fn new() -> Self {
        let (mut sender, mut receiver) = connect_media_peers();
        let mid = "audio".into();
        let ssrc: Ssrc = 42.into();

        sender.direct_api().declare_media(mid, MediaKind::Audio);
        sender.direct_api().declare_stream_tx(ssrc, None, mid, None);
        receiver.direct_api().declare_media(mid, MediaKind::Audio);

        let now = sender.last.max(receiver.last);
        sender.last = now;
        receiver.last = now;

        let params = sender.params_opus();
        assert_eq!(params.spec().codec, Codec::Opus);
        let payload_type = params.pt();
        let ssrc = sender
            .direct_api()
            .stream_tx_by_mid(mid, None)
            .unwrap()
            .ssrc();

        Self {
            sender,
            receiver,
            payload_type,
            ssrc,
            sequence_number: 1,
            rtp_time: 0,
        }
    }

    fn send(&mut self, payload: impl Into<SharedBytes>) {
        let wallclock = self.sender.start + self.sender.duration();
        {
            let mut direct = self.sender.direct_api();
            let stream = direct.stream_tx(&self.ssrc).unwrap();
            stream.write_rtp(RtpWrite::new(
                self.payload_type,
                self.sequence_number.into(),
                self.rtp_time,
                wallclock,
                payload,
            ));
        }

        self.sequence_number += 1;
        self.rtp_time = self.rtp_time.wrapping_add(RTP_TIME_STEP);
        progress(&mut self.sender, &mut self.receiver).expect("RTP progress");
    }

    fn take_rtp_payload(&mut self) -> SharedBytes {
        let payload = self
            .receiver
            .events
            .iter()
            .find_map(|(_, event)| match event {
                Event::RtpPacket(packet) => Some(packet.payload.clone()),
                _ => None,
            })
            .expect("encrypted RTP packet should produce an RtpPacket event");

        self.sender.events.clear();
        self.receiver.events.clear();
        payload
    }
}

struct Egress {
    sender: TestRtc,
    payload_type: Pt,
    ssrc: Ssrc,
    sequence_number: u64,
    rtp_time: u32,
}

impl Egress {
    fn new() -> Self {
        let (mut sender, _receiver) = connect_media_peers();
        let mid = "audio".into();
        let ssrc: Ssrc = 42.into();

        sender.direct_api().declare_media(mid, MediaKind::Audio);
        sender.direct_api().declare_stream_tx(ssrc, None, mid, None);

        let params = sender.params_opus();
        assert_eq!(params.spec().codec, Codec::Opus);
        let payload_type = params.pt();
        let ssrc = sender
            .direct_api()
            .stream_tx_by_mid(mid, None)
            .unwrap()
            .ssrc();

        Self {
            sender,
            payload_type,
            ssrc,
            sequence_number: 1,
            rtp_time: 0,
        }
    }

    fn queue(&mut self, payload: impl Into<SharedBytes>) {
        let wallclock = self.sender.start + self.sender.duration();
        {
            let mut direct = self.sender.direct_api();
            let stream = direct.stream_tx(&self.ssrc).unwrap();
            stream.write_rtp(RtpWrite::new(
                self.payload_type,
                self.sequence_number.into(),
                self.rtp_time,
                wallclock,
                payload,
            ));
        }

        self.sequence_number += 1;
        self.rtp_time = self.rtp_time.wrapping_add(RTP_TIME_STEP);
    }

    fn flush(&mut self) {
        // This is an SFU egress: encrypt and consume the transmitted datagram
        // locally. There is intentionally no second Rtc to decrypt it; that
        // would benchmark a loopback peer rather than an SFU forwarding path.
        let now = self.sender.last;
        self.sender
            .span
            .in_scope(|| self.sender.rtc.handle_input(Input::Timeout(now)))
            .expect("RTP egress timeout");

        loop {
            let output = self
                .sender
                .span
                .in_scope(|| self.sender.rtc.poll_output())
                .expect("RTP egress output");
            match output {
                Output::Timeout(next) => {
                    self.sender.last = if next == self.sender.last {
                        self.sender.last + self.sender.forced_time_advance
                    } else {
                        self.sender.last.min(next)
                    };
                    break;
                }
                Output::Transmit(packet) => {
                    black_box(packet.contents.len());
                }
                Output::Event(event) => {
                    black_box(event);
                }
            }
        }
    }
}

struct SfuWorkload {
    upstream: Connection,
    downstream: Vec<Egress>,
    payload: Vec<u8>,
}

impl SfuWorkload {
    fn new(fanout: usize, payload_size: usize) -> Self {
        Self {
            upstream: Connection::new(),
            downstream: (0..fanout).map(|_| Egress::new()).collect(),
            payload: (0..payload_size).map(|n| n as u8).collect(),
        }
    }

    fn forward_one_packet(&mut self) -> usize {
        // The source-side slice represents an encoded frame handed to the SFU
        // by an application. SharedBytes owns the packet data once it enters
        // str0m, then downstream sends clone that owned payload.
        self.upstream.send(self.payload.as_slice());
        let payload = self.upstream.take_rtp_payload();
        assert_eq!(payload.len(), self.payload.len());

        // Queue all destinations before driving any of them. This is the
        // normal multi-Rtc SFU shape: all egress queues retain their payload
        // clone at once, so the benchmark exercises the same-core refcount
        // path while the fan-out is being constructed.
        for downstream in &mut self.downstream {
            downstream.queue(payload.clone());
        }
        for downstream in &mut self.downstream {
            downstream.flush();
        }

        black_box(self.downstream.len())
    }

    fn forward_one_packet_with_vec_copies(&mut self) -> usize {
        self.upstream.send(self.payload.as_slice());
        let payload = self.upstream.take_rtp_payload();
        assert_eq!(payload.len(), self.payload.len());

        for downstream in &mut self.downstream {
            // This models the pre-shared-payload SFU path: each destination
            // receives its own Vec copy instead of another SharedBytes clone.
            downstream.queue(payload.as_ref().to_vec());
        }
        for downstream in &mut self.downstream {
            downstream.flush();
        }

        black_box(self.downstream.len())
    }
}

struct EncryptedIngressFanout {
    upstream: Connection,
    payload: Vec<u8>,
}

impl EncryptedIngressFanout {
    fn new(payload_size: usize) -> Self {
        Self {
            upstream: Connection::new(),
            payload: (0..payload_size).map(|n| n as u8).collect(),
        }
    }

    fn receive_one_packet(&mut self) -> SharedBytes {
        // This is the actual inbound half of the SFU path: an encrypted RTP
        // packet is produced, passed through SRTP decrypt and RTP parsing, and
        // only then is its immutable payload shared with each destination.
        self.upstream.send(self.payload.as_slice());
        let payload = self.upstream.take_rtp_payload();
        assert_eq!(payload.len(), self.payload.len());
        payload
    }

    fn fanout_one_packet(&mut self, fanout: usize) -> usize {
        let payload = self.receive_one_packet();

        // Keep the clones alive together. An SFU normally queues each
        // destination's packet, so the reference count remains elevated while
        // fan-out is being built. Passing the collection through black_box
        // prevents the compiler from removing the clones while avoiding one
        // black_box call per clone, which would swamp the refcount operation.
        let clones: Vec<_> = (0..fanout).map(|_| payload.clone()).collect();
        black_box(clones);

        black_box(fanout)
    }

    fn fanout_one_packet_with_vec_copies(&mut self, fanout: usize) -> usize {
        let payload = self.receive_one_packet();

        let copies: Vec<Vec<u8>> = (0..fanout).map(|_| payload.as_ref().to_vec()).collect();
        black_box(copies);

        black_box(fanout)
    }
}

fn benchmark_encrypted_sfu_forwarding(c: &mut Criterion) {
    init_crypto_default();

    let mut group = c.benchmark_group("encrypted_sfu_forwarding");

    for payload_size in [160, PAYLOAD_SIZE, 1350] {
        for fanout in [1, 2, 10, 50] {
            group.throughput(Throughput::Bytes((payload_size * (fanout + 1)) as u64));
            group.bench_function(
                BenchmarkId::new(format!("rtp_{payload_size}_bytes"), fanout),
                |b| {
                    let mut workload = SfuWorkload::new(fanout, payload_size);
                    b.iter(|| workload.forward_one_packet());
                },
            );
        }
    }

    group.finish();
}

fn benchmark_encrypted_rtp_fanout(c: &mut Criterion) {
    init_crypto_default();

    let mut group = c.benchmark_group("encrypted_rtp_payload_fanout");

    for payload_size in [160, PAYLOAD_SIZE, 1350] {
        for fanout in [1, 2, 10, 50] {
            group.throughput(Throughput::Bytes((payload_size * fanout) as u64));
            group.bench_function(
                BenchmarkId::new(format!("rtp_{payload_size}_bytes"), fanout),
                |b| {
                    let mut workload = EncryptedIngressFanout::new(payload_size);
                    b.iter(|| workload.fanout_one_packet(fanout));
                },
            );
        }
    }

    group.finish();
}

fn benchmark_vec_copy_comparison(c: &mut Criterion) {
    init_crypto_default();

    let mut group = c.benchmark_group("encrypted_vec_copy_comparison");

    for payload_size in [160, PAYLOAD_SIZE, 1350] {
        for fanout in [1, 2, 10, 50] {
            let bytes = (payload_size * fanout) as u64;
            group.throughput(Throughput::Bytes(bytes));

            group.bench_function(
                BenchmarkId::new(format!("shared_ingress_{payload_size}_bytes"), fanout),
                |b| {
                    let mut workload = EncryptedIngressFanout::new(payload_size);
                    b.iter(|| workload.fanout_one_packet(fanout));
                },
            );
            group.bench_function(
                BenchmarkId::new(format!("vec_copy_ingress_{payload_size}_bytes"), fanout),
                |b| {
                    let mut workload = EncryptedIngressFanout::new(payload_size);
                    b.iter(|| workload.fanout_one_packet_with_vec_copies(fanout));
                },
            );

            group.bench_function(
                BenchmarkId::new(format!("shared_relay_{payload_size}_bytes"), fanout),
                |b| {
                    let mut workload = SfuWorkload::new(fanout, payload_size);
                    b.iter(|| workload.forward_one_packet());
                },
            );
            group.bench_function(
                BenchmarkId::new(format!("vec_copy_relay_{payload_size}_bytes"), fanout),
                |b| {
                    let mut workload = SfuWorkload::new(fanout, payload_size);
                    b.iter(|| workload.forward_one_packet_with_vec_copies());
                },
            );
        }
    }

    group.finish();
}

fn profile_parameter(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

fn run_fixed_profile(name: &str, iterations: usize, mut operation: impl FnMut()) {
    println!("VALGRIND_READY profile={name} iterations={iterations}");
    std::io::stdout().flush().unwrap();
    std::thread::sleep(Duration::from_millis(profile_parameter(
        "SHARED_BYTES_VALGRIND_DELAY_MS",
        5_000,
    ) as u64));

    let started = Instant::now();
    for _ in 0..iterations {
        operation();
    }
    println!(
        "VALGRIND_ELAPSED_NS profile={name} elapsed_ns={}",
        started.elapsed().as_nanos()
    );
}

fn run_valgrind_profile(name: &str) {
    init_crypto_default();

    let payload_size = profile_parameter("SHARED_BYTES_VALGRIND_PAYLOAD", PAYLOAD_SIZE);
    let fanout = profile_parameter("SHARED_BYTES_VALGRIND_FANOUT", 50);
    let iterations = profile_parameter("SHARED_BYTES_VALGRIND_ITERATIONS", 100);

    match name {
        "shared-ingress" => {
            let mut workload = EncryptedIngressFanout::new(payload_size);
            run_fixed_profile(name, iterations, || {
                black_box(workload.fanout_one_packet(fanout));
            });
        }
        "vec-ingress" => {
            let mut workload = EncryptedIngressFanout::new(payload_size);
            run_fixed_profile(name, iterations, || {
                black_box(workload.fanout_one_packet_with_vec_copies(fanout));
            });
        }
        "shared-relay" => {
            let mut workload = SfuWorkload::new(fanout, payload_size);
            run_fixed_profile(name, iterations, || {
                black_box(workload.forward_one_packet());
            });
        }
        "vec-relay" => {
            let mut workload = SfuWorkload::new(fanout, payload_size);
            run_fixed_profile(name, iterations, || {
                black_box(workload.forward_one_packet_with_vec_copies());
            });
        }
        _ => panic!(
            "unknown SHARED_BYTES_VALGRIND profile {name:?}; expected shared-ingress, \
             vec-ingress, shared-relay, or vec-relay"
        ),
    }

    println!("VALGRIND_DONE profile={name}");
}

fn main() {
    if let Ok(profile) = std::env::var("SHARED_BYTES_VALGRIND") {
        run_valgrind_profile(&profile);
        return;
    }

    let mut criterion = Criterion::default().configure_from_args();
    benchmark_encrypted_sfu_forwarding(&mut criterion);
    benchmark_encrypted_rtp_fanout(&mut criterion);
    benchmark_vec_copy_comparison(&mut criterion);
    criterion.final_summary();
}
