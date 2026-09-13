// ---------------------------------------------------------------------------
// Protocol encode must not run on the connection task (`ChunkEncoder`).
// ---------------------------------------------------------------------------

// Set on any thread that has run [`ServerProtocol::decode`] — i.e. on the
// connection task's thread, and nowhere else.
//
// This is the discriminator [`EncodeSiteProto`] is built on, and it is exact
// rather than heuristic. `decode` is only ever called from
// `serve_connection_inner`/`dispatch_play_packet`, both of which run on the
// connection task; the tests below run on `#[tokio::test]`'s **current-thread**
// runtime — the same flavour production builds
// (`crates/lodestone-shell/src/net.rs`'s `new_current_thread`) — so that task
// cannot migrate, and `tokio`'s blocking pool is a disjoint set of threads that
// never decodes anything.
//
// Deliberately not a thread-id comparison: a thread id would have to be captured
// somewhere and compared somewhere else, and the capture site is exactly what a
// A flag set by `decode` itself directly identifies a connection thread.
thread_local! {
    static HAS_DECODED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Where column encode actually happened, counted by site.
#[derive(Debug, Default)]
struct EncodeSites {
    /// Encodes that ran on a thread which has served inbound packets — work that
    /// blocks the player's next interaction. Every one is ≈2.4 ms / 62 M instructions.
    /// next interaction waits behind.
    on_connection_thread: std::sync::atomic::AtomicUsize,
    /// Encodes that ran on a blocking-pool thread.
    off_connection_thread: std::sync::atomic::AtomicUsize,
}

impl EncodeSites {
    fn record(&self) {
        let counter = if HAS_DECODED.with(std::cell::Cell::get) {
            &self.on_connection_thread
        } else {
            &self.off_connection_thread
        };
        counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }

    fn on_task(&self) -> usize {
        self.on_connection_thread
            .load(std::sync::atomic::Ordering::SeqCst)
    }

    fn off_task(&self) -> usize {
        self.off_connection_thread
            .load(std::sync::atomic::Ordering::SeqCst)
    }
}

/// The `ChunkEncoder` half of [`EncodeSiteProto`], sharing its counters.
struct SiteEncoder {
    sites: std::sync::Arc<EncodeSites>,
}

impl lodestone_server::ChunkEncoder for SiteEncoder {
    fn encode_chunk(&self, cx: i32, cz: i32, column: &ChunkColumn) -> ServerDirective {
        self.sites.record();
        encode_site_chunk(cx, cz, column)
    }
}

/// The one column-encode body both arms use, so the arms differ in **where** the
/// encode runs and in nothing else. Payload is just the coordinate pair, which is
/// what [`collect_join_chunks`] reads.
fn encode_site_chunk(cx: i32, cz: i32, _column: &ChunkColumn) -> ServerDirective {
    let mut w = Writer::default();
    w.var_i32(cx);
    w.var_i32(cz);
    w.var_i32(0);
    ServerDirective::Send {
        packet_id: CHUNK,
        payload: w.as_slice().to_vec(),
    }
}

/// [`FakeProtocol`] plus an encode-site census, and a switch for whether it
/// offers an off-task [`lodestone_server::ChunkEncoder`] at all.
///
/// `off_task: false` is the **live negative control**: it is not a neutered copy
/// of the off-task arm, it is the shape every protocol without a `ChunkEncoder`
/// still has (the trait method defaults to `None`), driven through the same
/// `serve_connection` body. So the control cannot rot, and it must show the
/// and therefore exercises the fallback path.
struct EncodeSiteProto {
    sites: std::sync::Arc<EncodeSites>,
    off_task: bool,
}

impl ServerProtocol for EncodeSiteProto {
    fn decode(&self, state: State, packet_id: i32, payload: &[u8]) -> ServerBound {
        HAS_DECODED.with(|flag| flag.set(true));
        FakeProtocol.decode(state, packet_id, payload)
    }
    fn login_success(&self, username: &str, uuid: Uuid) -> Vec<ServerDirective> {
        FakeProtocol.login_success(username, uuid)
    }
    fn begin_configuration(&self) -> Vec<ServerDirective> {
        FakeProtocol.begin_configuration()
    }
    fn begin_play(&self, view_radius: i32) -> Vec<ServerDirective> {
        FakeProtocol.begin_play(view_radius)
    }
    fn begin_chunk_batch(&self) -> ServerDirective {
        FakeProtocol.begin_chunk_batch()
    }
    fn encode_chunk(&self, cx: i32, cz: i32, column: &ChunkColumn) -> ServerDirective {
        self.sites.record();
        encode_site_chunk(cx, cz, column)
    }
    fn chunk_encoder(&self) -> Option<std::sync::Arc<dyn lodestone_server::ChunkEncoder>> {
        self.off_task.then(|| {
            std::sync::Arc::new(SiteEncoder {
                sites: std::sync::Arc::clone(&self.sites),
            }) as std::sync::Arc<dyn lodestone_server::ChunkEncoder>
        })
    }
    fn end_chunk_batch(&self, batch_size: i32) -> ServerDirective {
        FakeProtocol.end_chunk_batch(batch_size)
    }
    fn encode_keep_alive(&self, id: i64) -> ServerDirective {
        FakeProtocol.encode_keep_alive(id)
    }
    fn encode_set_time(&self, game_time: i64, day_time: Option<i64>) -> ServerDirective {
        FakeProtocol.encode_set_time(game_time, day_time)
    }
    fn encode_chunk_cache_center(&self, cx: i32, cz: i32) -> ServerDirective {
        FakeProtocol.encode_chunk_cache_center(cx, cz)
    }
    fn encode_forget_chunk(&self, cx: i32, cz: i32) -> ServerDirective {
        FakeProtocol.encode_forget_chunk(cx, cz)
    }
    fn encode_change_difficulty(&self, difficulty: Difficulty, locked: bool) -> ServerDirective {
        FakeProtocol.encode_change_difficulty(difficulty, locked)
    }
}

/// Joins a loopback server built on `off_task`, sends one play packet before
/// reading a single chunk, and reports `(encodes on the connection thread when
/// the reply arrived, sites over the whole view, chunks observed)`.
async fn measure_encode_sites(
    off_task: bool,
    view_radius: i32,
) -> (usize, std::sync::Arc<EncodeSites>, usize) {
    let expected_chunks = ((2 * view_radius + 1) * (2 * view_radius + 1)) as usize;
    let sites = std::sync::Arc::new(EncodeSites::default());
    let generated = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));

    let server = lodestone_server::IntegratedServer::bind(
        "127.0.0.1:0",
        EncodeSiteProto {
            sites: std::sync::Arc::clone(&sites),
            off_task,
        },
        CountingAirSource {
            generated: std::sync::Arc::clone(&generated),
        },
        view_radius,
    )
    .await
    .expect("bind loopback");
    let addr = server.local_addr().expect("a bound server has an address");

    let mut client = Connection::new(
        tokio::net::TcpStream::connect(addr)
            .await
            .expect("client connects"),
    );
    client.write_packet(HANDSHAKE, &[2]).await.expect("hs");
    let mut w = Writer::default();
    w.string(&format!("EncodeSite{}", u8::from(off_task)));
    client
        .write_packet(LOGIN_START, w.as_slice())
        .await
        .expect("login start");
    client.read_packet().await.unwrap().unwrap(); // LOGIN_SUCCESS
    client
        .write_packet(LOGIN_ACKNOWLEDGED, &[])
        .await
        .expect("login ack");
    client
        .write_packet(FINISH_CONFIGURATION, &[])
        .await
        .expect("finish configuration");
    // The interaction, written before a single chunk has been read — exactly as
    // `a_play_packet_is_serviced_before_the_last_join_chunk` does, and for the
    // same reason: it is the cheapest packet in this file's vocabulary with an
    // observable reply.
    client
        .write_packet(CHANGE_DIFFICULTY_C2S, &[3])
        .await
        .expect("difficulty change");

    let mut on_task_at_reply = None;
    let mut observed = 0usize;
    while observed < expected_chunks {
        let (id, _payload) = client
            .read_packet()
            .await
            .expect("read")
            .expect("the server must not close mid-view");
        if id == CHUNK {
            observed += 1;
        } else if id == CHANGE_DIFFICULTY_S2C && on_task_at_reply.is_none() {
            on_task_at_reply = Some(sites.on_task());
        }
    }

    drop(client);
    server.shutdown().await;

    let at = on_task_at_reply.expect(
        "the difficulty change was never answered: the play loop either never ran or the \
         packet was consumed without a reply",
    );
    // Returned as the `Arc`: the serving task still holds the protocol at this
    // point (`shutdown` does not join it synchronously), so a `try_unwrap` here
    // panics on a live second reference and says nothing about the counters.
    (at, sites, observed)
}

/// **The latency claim as a counter: how many columns' worth of encode work can
/// sit between a play packet and its reply.**
///
/// `crate::protocol::ChunkEncoder`'s measurement is 62 M instructions / ≈2.4 ms
/// per column. Running that work on the connection task delays the acting
/// player's answer. A wall clock cannot
/// state that here (this machine's timings reproduce to ~10.8% and several agents
/// compile concurrently), so the instrument is a **count of encodes that ran on a
/// thread which has served an inbound packet**; see [`HAS_DECODED`] for why that
/// is exact rather than a proxy.
///
/// | arm | encodes on the connection thread |
/// |---|---|
/// | no `ChunkEncoder` (the live control below) | **every column of the view** — 361 at `view_radius = 9` |
/// | an off-task `ChunkEncoder` | **0** |
///
/// The two arms differ in one thing: whether the protocol answers
/// `chunk_encoder()` with `Some`. Same server body, same source, same wire.
///
/// The exact-zero is what makes this a magnitude assertion rather than a
/// direction one: it is not "fewer", and no reordering of the burst can satisfy
/// it. And the view still has to arrive whole, which the last assertion checks —
/// dropping columns would otherwise trivially pass.
#[tokio::test]
async fn column_encode_never_runs_on_the_connection_task() {
    let view_radius = 9;
    let expected = ((2 * view_radius + 1) * (2 * view_radius + 1)) as usize;

    let (at_reply, sites, observed) = measure_encode_sites(true, view_radius).await;
    assert_eq!(
        at_reply, 0,
        "{at_reply} columns had been encoded on the connection task by the time the play \
         packet was answered; with encode moved into the generating worker this is 0, and \
         under the defect it is however many chunks had gone out"
    );
    assert_eq!(
        sites.on_task(),
        0,
        "not one of the view's {expected} columns may be encoded on a thread that serves \
         inbound packets — that is the whole of `ChunkEncoder`"
    );
    assert_eq!(
        sites.off_task(),
        expected,
        "…and every column must still be encoded exactly once, on the blocking pool"
    );
    assert_eq!(observed, expected, "the whole view must still arrive");
}

/// **The live control, and it must fail the assertion above.**
///
/// A protocol that answers [`ServerProtocol::chunk_encoder`] with `None` — the
/// trait default, and what every legacy family and every other test protocol in
/// this workspace does — drives the identical `serve_connection` body down the
/// on-task encode path. If this arm ever reported zero on-task encodes, the gate
/// beside it would be measuring nothing: an encode site that is *never* on the
/// connection task regardless of the selected encoder.
///
/// Asserted as "every column", not "some": the fallback path encodes the whole
/// view on the connection task, so a partial count would mean the two paths had
/// silently merged.
#[tokio::test]
async fn control_without_an_off_task_encoder_every_column_is_encoded_on_the_connection_task() {
    let view_radius = 9;
    let expected = ((2 * view_radius + 1) * (2 * view_radius + 1)) as usize;

    let (at_reply, sites, observed) = measure_encode_sites(false, view_radius).await;
    assert_eq!(
        sites.on_task(),
        expected,
        "the control must show the defect: with no off-task encoder all {expected} columns \
         are encoded on the connection task"
    );
    assert_eq!(
        sites.off_task(),
        0,
        "and none off it — a non-zero here means the two arms are not actually different"
    );
    assert!(
        at_reply > 0,
        "the control's reply must arrive with on-task encode work already behind it, or the \
         `at_reply == 0` assertion in the fixed arm is vacuous"
    );
    assert_eq!(observed, expected, "the whole view must still arrive");
}
