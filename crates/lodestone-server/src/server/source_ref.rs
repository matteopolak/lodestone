//! `SourceRef`, the borrowed-or-shared handle to a connection's chunk source.

use super::*;

/// How a connection reaches its terrain, including the blocking and offloaded
/// blocking-vs-offloaded fork in one place.
///
/// Chunk generation is CPU-bound and synchronous, so it has to be moved off
/// the async runtime's core thread — see
/// [`generate_columns_offloaded`](crate::chunk::generate_columns_offloaded)
/// for the measurement and for why `spawn_blocking` rather than
/// `block_in_place`. `spawn_blocking` needs a `'static` closure, which a
/// `&S` cannot provide. That normally forces `serve_connection`'s `source`
/// parameter from `&S` to `Arc<S>`. The separate wrappers preserve the borrowed
/// source API while the shared variant can move an `Arc<S>` into a blocking
/// generation task.
///
/// This enum is how both shapes share one body instead:
///
/// | arm | generation | who uses it |
/// |---|---|---|
/// | [`Shared`](Self::Shared) | offloaded, never blocks the runtime | every production caller in [`crate::integrated`] |
/// | [`Borrowed`](Self::Borrowed) | blocking, direct generation | `&S`-shaped test call sites |
///
/// The `Borrowed` arm is deliberately kept rather than deleted: it is the
/// **permanent negative control** for the offloading gate. A test can drive the exact
/// same `serve_connection` body down the blocking path and watch the world
/// tick stall, which is what proves the `Shared` arm's non-stall assertion is
/// measuring something. A control that only exists as a temporary neuter
/// cannot be re-run later.
///
/// `Copy` (hand-written, because `#[derive(Copy)]` would demand `S: Copy`)
/// so it threads through the dispatch chain exactly as cheaply as the `&S`
/// it replaces.
///
/// Portal travel uses the [`Dimension`](Self::Dimension) arm.
///
/// `Debug` is hand-written because:
/// `#[derive(Debug)]` would demand `dyn ChunkSource: Debug`, and making `Debug` a
/// supertrait of `ChunkSource` to satisfy a diagnostic impl is the wrong direction.
pub(crate) enum SourceRef<'a, S> {
    /// A plain borrow. Generation blocks the calling thread.
    Borrowed(&'a S),
    /// A shared handle. Generation is offloaded to the blocking pool.
    Shared(&'a Arc<S>),
    /// **Another dimension's** terrain, reached through
    /// [`ChunkSource::sibling`](crate::ChunkSource::sibling) after a portal trip.
    /// Generation is offloaded exactly as [`Shared`](Self::Shared) is.
    ///
    /// # Why this is not just `Shared`
    ///
    /// The Nether's concrete source type need not be the overworld's (the
    /// primary `S` is whatever the caller built, while the Nether is a
    /// `ChunkStore` over the 26.3 generator inside its own `DimensionalSource`),
    /// so no single `S` can name both — `Shared(&'a Arc<S>)` is monomorphic in the connection's `S` by
    /// construction. Erasing to `dyn ChunkSource` here is what lets a connection
    /// change dimension without the whole `serve_play` state machine being generic
    /// over which dimension it is in.
    ///
    /// This is also why [`get`](Self::get) hands back `&dyn ChunkSource` rather
    /// than `&S`: every helper it feeds is `S: ChunkSource + ?Sized`, and the
    /// `?Sized` bounds throughout this file exist for exactly this arm.
    Dimension(&'a Arc<dyn ChunkSource>),
}

impl<S> Clone for SourceRef<'_, S> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<S> Copy for SourceRef<'_, S> {}

impl<S> std::fmt::Debug for SourceRef<'_, S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let arm = match self {
            Self::Borrowed(_) => "Borrowed",
            Self::Shared(_) => "Shared",
            Self::Dimension(_) => "Dimension",
        };
        f.debug_tuple("SourceRef").field(&arm).finish()
    }
}

impl<'a, S: ChunkSource + 'static> SourceRef<'a, S> {
    /// The underlying source, for the read/write paths that never generate a
    /// whole batch (`block_state`, `set_block`) and so have nothing to
    /// offload.
    pub(super) fn get(self) -> &'a dyn ChunkSource {
        match self {
            Self::Borrowed(source) => source,
            Self::Shared(source) => &**source,
            Self::Dimension(source) => &**source,
        }
    }

    pub(super) fn shared_arc(self) -> Option<Arc<dyn ChunkSource>> {
        match self {
            Self::Borrowed(_) => None,
            Self::Shared(source) => {
                let owned: Arc<S> = Arc::clone(source);
                Some(owned)
            }
            Self::Dimension(source) => Some(Arc::clone(source)),
        }
    }

    /// Which dimension this reference reads, treating an unlabelled source as the
    /// overworld — see [`ChunkSource::dimension`](crate::ChunkSource::dimension)
    /// for why `None` is a distinct answer at the trait but collapses here.
    pub(super) fn dimension(self) -> crate::dimension::Dimension {
        self.get()
            .dimension()
            .unwrap_or(crate::dimension::Dimension::Overworld)
    }

    /// Generates every column in `coords`, in `coords` order — off the core
    /// thread on the [`Shared`](Self::Shared) arm, on it for
    /// [`Borrowed`](Self::Borrowed).
    ///
    /// The ordering guarantee is the same one
    /// [`generate_columns_parallel`] documents, and it is load-bearing for
    /// the wire: both arms hand back a `Vec` aligned index-for-index with
    /// `coords`, so which arm a caller is on cannot change the emitted byte
    /// sequence. On wasm32 the borrowed arm uses the same generator one
    /// column at a time and yields to the browser between columns; it never
    /// enters the native thread pool.
    /// `pub(crate)` rather than private since `crate::join_scheduler`'s
    /// `JoinChunkStream` finishes a join from `serve_play`, and its borrowed arm
    /// generates through exactly this method — the alternative was duplicating the
    /// arm fork, which is the one thing that must not drift.
    pub(crate) async fn generate(
        self,
        coords: Vec<(i32, i32)>,
    ) -> Result<Vec<ChunkColumn>, ChunkEncodeError> {
        match self {
            Self::Shared(source) => {
                let source: Arc<dyn ChunkSource> = source.clone();
                crate::join_scheduler::generate_owned_columns(source, coords).await
            }
            #[cfg(not(target_arch = "wasm32"))]
            Self::Borrowed(source) => Ok(generate_columns_parallel(source, &coords)),
            #[cfg(target_arch = "wasm32")]
            Self::Borrowed(source) => Ok(generate_columns_borrowed(source, &coords).await),
            Self::Dimension(source) => {
                crate::join_scheduler::generate_owned_columns(Arc::clone(source), coords).await
            }
        }
    }

    /// Admits a set of columns before a synchronous consumer touches them.
    ///
    /// The integrated source is the `Shared` arm, so the actual `column()`
    /// calls run on the world-generation worker pool. Keeping this broker on
    /// `SourceRef` makes the ordering explicit at each connection boundary:
    /// the future does not resolve until every requested coordinate has had a
    /// chance to enter the source's resident cache, while an already-resident
    /// coordinate remains a cheap cache hit.
    pub(super) async fn admit_columns(self, coords: Vec<(i32, i32)>) -> Result<(), ChunkEncodeError> {
        self.generate(coords).await.map(|_| ())
    }

    /// Resolves a fresh world's initial spawn without blocking the connection
    /// runtime when its terrain is shared with an integrated server.
    ///
    /// `find_initial_spawn` probes a spiral of columns and its `ChunkSource`
    /// interface is intentionally synchronous. That is safe for borrowed
    /// fixture sources, but a shared integrated source is normally served by
    /// the shell's current-thread runtime. Calling it directly there prevents
    /// the same runtime from progressing the first chunk stream until the
    /// entire spawn search completes. Keep the borrowed arm as the synchronous
    /// control path while sending production shared sources through the
    /// world-generation dispatcher as [`Self::generate`] does.
    pub(super) async fn find_initial_spawn(self) -> crate::world_spawn::WorldSpawn {
        match self {
            Self::Borrowed(source) => crate::world_spawn::find_initial_spawn(source),
            Self::Shared(source) => {
                #[cfg(target_arch = "wasm32")]
                {
                    return crate::world_spawn::find_initial_spawn_yielding(&**source).await;
                }
                #[cfg(not(target_arch = "wasm32"))]
                let source = Arc::clone(source);
                #[cfg(not(target_arch = "wasm32"))]
                crate::spawn::spawn_worldgen(move || {
                    crate::world_spawn::find_initial_spawn(&*source)
                })
                .await
            }
            Self::Dimension(source) => {
                #[cfg(target_arch = "wasm32")]
                {
                    return crate::world_spawn::find_initial_spawn_yielding(&**source).await;
                }
                #[cfg(not(target_arch = "wasm32"))]
                let source = Arc::clone(source);
                #[cfg(not(target_arch = "wasm32"))]
                crate::spawn::spawn_worldgen(move || {
                    crate::world_spawn::find_initial_spawn(&*source)
                })
                .await
            }
        }
    }
}
