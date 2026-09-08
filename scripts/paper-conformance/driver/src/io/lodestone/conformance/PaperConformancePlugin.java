package io.lodestone.conformance;

import java.util.UUID;

import org.bukkit.Bukkit;
import org.bukkit.GameMode;
import org.bukkit.Location;
import org.bukkit.Material;
import org.bukkit.World;
import org.bukkit.WorldCreator;
import org.bukkit.block.Block;
import org.bukkit.entity.Player;
import org.bukkit.event.EventHandler;
import org.bukkit.event.Listener;
import org.bukkit.event.block.BlockBreakEvent;
import org.bukkit.event.player.PlayerJoinEvent;
import org.bukkit.plugin.java.JavaPlugin;

/**
 * Minimal operator-built Paper driver for paper-observation-v1.
 *
 * The plugin never constructs or calls a BlockBreakEvent. A real player must
 * break the fixed target twice: the registered listener cancels the first
 * break, then unregisters itself; a scheduler observes the second real break
 * changing the target to air before the result is emitted.
 */
public final class PaperConformancePlugin extends JavaPlugin implements Listener {
    private static final String WORLD_NAME = "paper-conformance";
    private static final long SEED = 730L;
    private static final int TARGET_X = 1;
    private static final int TARGET_Y = 64;
    private static final int TARGET_Z = 0;
    private static final String OBSERVATION =
            "{\"schema\":1,\"kind\":\"observation\",\"backend\":\"paper\","
                    + "\"scenario\":\"block-break-cancel\",\"status\":\"complete\","
                    + "\"evidence_kind\":\"external\","
                    + "\"source\":\"operator-paper-conformance-driver\","
                    + "\"observations\":["
                    + "{\"id\":\"listener-present\",\"outcome\":\"cancelled\"},"
                    + "{\"id\":\"no-listener\",\"outcome\":\"completed\"}]}";

    private World world;
    private boolean listenerPresent;
    private boolean firstBreakObserved;
    private int pollTask = -1;

    @Override
    public void onEnable() {
        world = Bukkit.getWorld(WORLD_NAME);
        if (world == null) {
            world = new WorldCreator(WORLD_NAME).seed(SEED).createWorld();
        }
        if (world == null) {
            throw new IllegalStateException("could not create conformance world");
        }
        world.getBlockAt(TARGET_X, TARGET_Y, TARGET_Z).setType(Material.STONE, false);
        listenerPresent = true;
        Bukkit.getPluginManager().registerEvents(this, this);
        pollTask = Bukkit.getScheduler().runTaskTimer(this, this::observeSecondBreak, 1L, 1L).getTaskId();
        getLogger().info("paper-observation-v1 ready: have a player break the target twice");
    }

    @EventHandler
    public void onPlayerJoin(PlayerJoinEvent event) {
        preparePlayer(event.getPlayer());
    }

    @EventHandler
    public void onBlockBreak(BlockBreakEvent event) {
        if (!listenerPresent || !isTarget(event.getBlock())) {
            return;
        }
        firstBreakObserved = true;
        event.setCancelled(true);
        listenerPresent = false;
        // The second control deliberately has no BlockBreakEvent listener.
        org.bukkit.event.HandlerList.unregisterAll(this);
        event.getPlayer().sendMessage("Break the conformance target once more for the no-listener control.");
        getLogger().info("listener-present control observed and cancelled");
    }

    private void preparePlayer(Player player) {
        if (world == null || firstBreakObserved) {
            return;
        }
        player.teleport(new Location(world, TARGET_X + 0.5, TARGET_Y + 1.0, TARGET_Z + 2.5));
        player.setGameMode(GameMode.SURVIVAL);
        player.sendMessage("Break the stone block at (1,64,0); then break it again after cancellation.");
    }

    private void observeSecondBreak() {
        if (!firstBreakObserved || listenerPresent) {
            return;
        }
        Block target = world.getBlockAt(TARGET_X, TARGET_Y, TARGET_Z);
        if (target.getType() != Material.AIR) {
            return;
        }
        if (pollTask >= 0) {
            Bukkit.getScheduler().cancelTask(pollTask);
        }
        System.out.println(OBSERVATION);
        System.out.flush();
        Bukkit.getScheduler().runTask(this, Bukkit::shutdown);
    }

    private boolean isTarget(Block block) {
        return block.getWorld().equals(world)
                && block.getX() == TARGET_X
                && block.getY() == TARGET_Y
                && block.getZ() == TARGET_Z;
    }
}
