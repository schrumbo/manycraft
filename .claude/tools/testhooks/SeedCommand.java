package net.minecraft.server.commands;

import com.mojang.brigadier.CommandDispatcher;
import net.minecraft.commands.CommandSourceStack;
import net.minecraft.commands.Commands;
import net.minecraft.commands.arguments.EntityArgument;
import net.minecraft.commands.arguments.coordinates.BlockPosArgument;
import net.minecraft.core.component.DataComponents;
import net.minecraft.network.chat.Component;
import net.minecraft.network.chat.ComponentUtils;
import net.minecraft.server.level.ServerLevel;
import net.minecraft.server.level.ServerPlayer;
import net.minecraft.world.InteractionHand;
import net.minecraft.world.MenuProvider;
import net.minecraft.world.item.ItemStack;
import net.minecraft.world.item.Items;
import net.minecraft.world.level.Level;
import net.minecraft.world.level.saveddata.maps.MapId;
import net.minecraft.world.level.saveddata.maps.MapItemSavedData;

/**
 * Test-only replacement of the vanilla seed command, put ahead of the server jar by {@code testserver start --hooks}.
 * Keeps {@code /seed} and adds what a test needs that vanilla cannot do without the player's input:
 * {@code /testopen <player> <pos>} opens the container at pos for the player, and {@code /testmap <player> <digits>}
 * puts a locked map with the digits drawn on it into the player's main hand, like a map captcha.
 */
public class SeedCommand {
    private static final String[] FONT = {"111101101101111", "010110010010111", "111001111100111", "111001111001111", "101101111001001", "111100111001111", "111100111101111", "111001001001001", "111101111101111", "111101111001111"};

    public static void register(final CommandDispatcher<CommandSourceStack> dispatcher, final boolean checkPermissions) {
        dispatcher.register(Commands.literal("seed").requires(Commands.hasPermission(checkPermissions ? Commands.LEVEL_GAMEMASTERS : Commands.LEVEL_ALL)).executes(c -> {
            long seed = c.getSource().getLevel().getSeed();
            Component seedText = ComponentUtils.copyOnClickText(String.valueOf(seed));
            c.getSource().sendSuccess(() -> Component.translatable("commands.seed.success", seedText), false);
            return (int) seed;
        }));
        dispatcher.register(Commands.literal("testopen").requires(Commands.hasPermission(Commands.LEVEL_GAMEMASTERS))
            .then(Commands.argument("player", EntityArgument.player()).then(Commands.argument("pos", BlockPosArgument.blockPos()).executes(c -> {
                ServerPlayer player = EntityArgument.getPlayer(c, "player");
                if (player.level().getBlockEntity(BlockPosArgument.getLoadedBlockPos(c, "pos")) instanceof MenuProvider provider) {
                    player.openMenu(provider);
                    return 1;
                }
                c.getSource().sendFailure(Component.literal("no container there"));
                return 0;
            }))));
        dispatcher.register(Commands.literal("testmap").requires(Commands.hasPermission(Commands.LEVEL_GAMEMASTERS))
            .then(Commands.argument("player", EntityArgument.player()).then(Commands.argument("digits", com.mojang.brigadier.arguments.StringArgumentType.word()).executes(c -> {
                ServerPlayer player = EntityArgument.getPlayer(c, "player");
                ServerLevel level = player.level();
                MapItemSavedData data = MapItemSavedData.createFresh(0, 0, (byte) 0, false, false, Level.OVERWORLD).locked();
                draw(data, com.mojang.brigadier.arguments.StringArgumentType.getString(c, "digits"));
                MapId id = level.getFreeMapId();
                level.setMapData(id, data);
                ItemStack map = new ItemStack(Items.FILLED_MAP);
                map.set(DataComponents.MAP_ID, id);
                player.setItemInHand(InteractionHand.MAIN_HAND, map);
                return id.id();
            }))));
    }

    private static void draw(final MapItemSavedData data, final String digits) {
        for (int x = 0; x < 128; x++) {
            for (int y = 0; y < 128; y++) {
                boolean edge = x < 4 || y < 4 || x > 123 || y > 123;
                data.setColor(x, y, (byte) (edge ? 28 * 4 + 2 : 8 * 4 + 2));
            }
        }
        int scale = 6;
        int left = (128 - digits.length() * 4 * scale + scale) / 2;
        for (int i = 0; i < digits.length(); i++) {
            int d = Character.digit(digits.charAt(i), 10);
            if (d < 0) {
                continue;
            }
            for (int row = 0; row < 5; row++) {
                for (int col = 0; col < 3; col++) {
                    if (FONT[d].charAt(row * 3 + col) != '1') {
                        continue;
                    }
                    for (int dx = 0; dx < scale; dx++) {
                        for (int dy = 0; dy < scale; dy++) {
                            int x = left + (i * 4 + col) * scale + dx;
                            int y = 49 + row * scale + dy;
                            if (x >= 0 && x < 128) {
                                data.setColor(x, y, (byte) (29 * 4 + 2));
                            }
                        }
                    }
                }
            }
        }
    }
}
