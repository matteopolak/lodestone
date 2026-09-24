package io.lodestone.conformance;

import java.util.Set;
import java.util.TreeSet;

import org.bukkit.Bukkit;
import org.bukkit.GameMode;
import org.bukkit.Location;
import org.bukkit.Material;
import org.bukkit.World;
import org.bukkit.WorldCreator;
import org.bukkit.block.Block;
import org.bukkit.entity.Player;
import org.bukkit.event.EventHandler;
import org.bukkit.event.EventPriority;
import org.bukkit.event.Listener;
import org.bukkit.event.block.BlockBreakEvent;
import org.bukkit.event.player.PlayerJoinEvent;
import org.bukkit.plugin.Plugin;
import org.bukkit.plugin.java.JavaPlugin;

/**
 * Minimal operator-built Paper driver for paper-observation-v1.
 *
 * The plugin never constructs or cancels a BlockBreakEvent. A real player must
 * break the fixed target twice: the monitor listener records whether a target
 * plugin cancelled the first break, then unregisters every other plugin's
 * listener; a scheduler observes the second real break changing the target to
 * air before the result is emitted.
 */
public final class PaperConformancePlugin extends JavaPlugin implements Listener {
    private static final String WORLD_NAME = "paper-conformance";
    private static final long SEED = 730L;
    private static final int TARGET_X = 1;
    private static final int TARGET_Y = 64;
    private static final int TARGET_Z = 0;
    private World world;
    private boolean firstBreakObserved;
    private boolean firstBreakCancelled;
    private boolean secondBreakObserved;
    private boolean secondBreakCancelled;
    private Player testPlayer;
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
        Bukkit.getPluginManager().registerEvents(this, this);
        pollTask = Bukkit.getScheduler().runTaskTimer(this, this::observeSecondBreak, 1L, 1L).getTaskId();
        getLogger().info("paper-observation-v1 ready: have a player break the target twice");
    }

    @EventHandler
    public void onPlayerJoin(PlayerJoinEvent event) {
        preparePlayer(event.getPlayer());
    }

    @EventHandler(priority = EventPriority.MONITOR)
    public void onBlockBreak(BlockBreakEvent event) {
        if (!isTarget(event.getBlock())) {
            return;
        }
        if (!firstBreakObserved) {
            firstBreakObserved = true;
            firstBreakCancelled = event.isCancelled();
            testPlayer = event.getPlayer();
            Bukkit.getScheduler().runTask(this, this::prepareNoListenerControl);
            getLogger().info("listener-present control observed: "
                    + (firstBreakCancelled ? "cancelled" : "completed"));
        } else if (!secondBreakObserved) {
            secondBreakObserved = true;
            secondBreakCancelled = event.isCancelled();
            getLogger().info("no-listener control observed: "
                    + (secondBreakCancelled ? "cancelled" : "completed"));
        }
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
        if (!secondBreakObserved) {
            return;
        }
        Block target = world.getBlockAt(TARGET_X, TARGET_Y, TARGET_Z);
        if (target.getType() != Material.AIR) {
            return;
        }
        if (pollTask >= 0) {
            Bukkit.getScheduler().cancelTask(pollTask);
        }
        System.out.println(observation(firstBreakCancelled, secondBreakCancelled));
        System.out.flush();
        Bukkit.getScheduler().runTask(this, Bukkit::shutdown);
    }

    private void prepareNoListenerControl() {
        for (Plugin plugin : Bukkit.getPluginManager().getPlugins()) {
            if (plugin != this) {
                org.bukkit.event.HandlerList.unregisterAll(plugin);
            }
        }
        Block target = world.getBlockAt(TARGET_X, TARGET_Y, TARGET_Z);
        if (target.getType() == Material.AIR) {
            target.setType(Material.STONE, false);
        }
        if (testPlayer != null) {
            testPlayer.sendMessage("Break the conformance target once more for the no-listener control.");
        }
    }

    private String observation(boolean firstCancelled, boolean secondCancelled) {
        Set<String> enabled = new TreeSet<>();
        for (Plugin plugin : Bukkit.getPluginManager().getPlugins()) {
            if (plugin.isEnabled()) {
                enabled.add(plugin.getName());
            }
        }
        StringBuilder names = new StringBuilder();
        boolean first = true;
        for (String name : enabled) {
            if (!first) {
                names.append(',');
            }
            first = false;
            names.append('"').append(escapeJson(name)).append('"');
        }
        return "{\"schema\":1,\"kind\":\"observation\",\"backend\":\"paper\","
                + "\"scenario\":\"block-break-cancel\",\"status\":\"complete\","
                + "\"evidence_kind\":\"external\","
                + "\"source\":\"operator-paper-conformance-driver\","
                + "\"enabled_plugins\":[" + names + "],"
                + "\"observations\":["
                + "{\"id\":\"listener-present\",\"outcome\":\""
                + (firstCancelled ? "cancelled" : "completed") + "\"},"
                + "{\"id\":\"no-listener\",\"outcome\":\""
                + (secondCancelled ? "cancelled" : "completed") + "\"}]}";
    }

    private static String escapeJson(String value) {
        return value.replace("\\", "\\\\").replace("\"", "\\\"");
    }

    private boolean isTarget(Block block) {
        return block.getWorld().equals(world)
                && block.getX() == TARGET_X
                && block.getY() == TARGET_Y
                && block.getZ() == TARGET_Z;
    }
}
