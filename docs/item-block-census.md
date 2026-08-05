# Item → block census

## What it is

The authoritative 26.2 answer to "if a player right-clicks holding this item, which block
gets placed?" — `lodestone_data::block_items`, generated from a headless dump of the real
server's `BuiltInRegistries.ITEM` joined with `BlockItem.getBlock()`.

It exists because block placement in `lodestone-server` used to answer that question with
`minecraft:stone` for every ordinary block (#466). It is the *block*, never the block
*state*.

## How it works

Three files, in dependency order:

| file | role |
|---|---|
| `crates/lodestone-data/oracle-java/BlockItemOracle.java` | boots the real 26.2 server headlessly, walks the item registry, prints one line per item |
| `crates/lodestone-data/tests/support/block_items_jvm.txt` | the committed dump — 1,537 rows, the external anchor |
| `crates/lodestone-data/src/generated/block_items.rs` | the generated `[Option<&str>; 1537]`, indexed by item network registry id |

`crates/lodestone-data/src/block_items.rs` is the public surface:

```rust
block_for_item_id(id: i32) -> Option<&'static str>   // O(1), the hot path
block_for_item(item: &str) -> Option<&'static str>   // resolves the name via items::item_id
is_block_item(item: &str) -> bool
```

`None` means "places no block" — a sword, a bucket, an unknown id. Callers that place
blocks treat all three the same way, which is why the reasons are not distinguished.

Of the 1,537 items, **1,054 are `BlockItem`s and 483 are not**.

### Why not just match the name

Because it is measurably wrong. Vanilla's `BlockItem` holds an explicit `Block`
reference and is registered under an item id that need not match it. Against the
committed dump, `item_name == block_name` disagrees on **16 of 1,537 items**, in both
directions:

* **14 false negatives** — a real placeable item it declines outright:
  `redstone`→`redstone_wire`, `string`→`tripwire`, `wheat_seeds`→`wheat`,
  `cocoa_beans`→`cocoa`, `carrot`→`carrots`, `potato`→`potatoes`,
  `pumpkin_seeds`→`pumpkin_stem`, `melon_seeds`→`melon_stem`, and others.
* **2 false positives** — `minecraft:air` and `minecraft:wheat`. A block of each name
  exists, but neither item is a `BlockItem`. `minecraft:wheat` is the crop's *drop*
  (`Items.java:1048`, a plain `registerItem`) while the block of that name is the crop
  itself, so a name match would grow a crop when the player holds wheat.

The false positives are the worse half: they place a block the player never asked for —
the same class of defect as writing stone for everything, just rarer.

Note `lodestone-shell`'s placement *predictor* (`sim/placement.rs`'s `block_states_of`)
still uses the name heuristic. That is a separate, client-side concern and declining is
safe there (it falls back to send-and-wait), but it carries the same 16-row error and is
a reasonable future consumer of this table.

## How to change it

Refreshing after a version bump is two steps, both documented at the top of
`crates/lodestone-data/tests/block_items.rs`:

1. Re-dump. Any JDK ≥ 21 with the server jar and its libraries on the classpath:

   ```bash
   MC="$(cd .cache/mc/26.2 && pwd)"
   CP="$MC/versions/26.2/server-26.2.jar:$(find "$MC/libraries" -name '*.jar' | tr '\n' ':')"
   javac -cp "$CP" -d /tmp/oracle crates/lodestone-data/oracle-java/BlockItemOracle.java
   java -cp "/tmp/oracle:$CP" BlockItemOracle
   ```

   Copy stdout over `tests/support/block_items_jvm.txt`, keeping the `#` header. The
   neighbouring oracles document a `eclipse-temurin:25-jdk` Docker classpath instead;
   either works, and a local JDK avoids needing the Docker VM at all.

2. Regenerate:

   ```bash
   LODESTONE_REGEN=1 cargo test -p lodestone-data --test block_items \
       committed_table_matches_the_committed_dump -- --ignored --nocapture
   ```

### Gotchas

* **Bootstrapping.** `src/generated/block_items.rs` must exist for the crate to compile
  before the generator (which is a *test*) can run. Creating the file new means writing a
  stub with `ITEM_COUNT: u32 = 0` and an empty array first, then regenerating over it.
* **Scope is `BlockItem`, deliberately.** Buckets placing fluids, spawn eggs and
  minecarts spawning entities, `flint_and_steel` lighting a fire — all report `None`.
  Each needs its own mechanism; folding them in would be a hand-written guess wearing
  generated clothes.
* **This answers the block, not the state.** Nothing here knows about `facing`, `axis`,
  `half`, or redstone connection state. A consumer needing a state id must resolve one
  itself.
* **Do not hand-edit the generated table.** The drift guard
  (`committed_table_matches_the_committed_dump`) regenerates from the dump and asserts
  byte equality, so a hand edit fails rather than ships.

## Configuration

`LODESTONE_REGEN=1` switches the drift guard from assert to rewrite. Nothing else.

## Dependencies

* `lodestone_data::items` — for the name→id reverse scan behind `block_for_item`.
* `lodestone_data::block_states` — used only by the tests, to prove every block this
  table names is a real registered block.
* The real 26.2 server jar under `.cache/mc/26.2/`, and a JDK, to re-dump.

Consumed by `lodestone-server`'s `apply_use_item_on`
(`crates/lodestone-server/src/server.rs`) — see [`block-edit.md`](./block-edit.md).
