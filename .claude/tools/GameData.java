import java.io.IOException;
import java.io.PrintWriter;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;

import net.minecraft.SharedConstants;
import net.minecraft.core.Registry;
import net.minecraft.core.registries.BuiltInRegistries;
import net.minecraft.server.Bootstrap;
import net.minecraft.world.item.BlockItem;
import net.minecraft.world.item.Item;
import net.minecraft.world.level.material.MapColor;

/**
 * Writes what the connection-only client needs from the game code itself, as tab-separated lines: the map colours
 * (packed id, ARGB), the items (protocol id, name, translation key, block map colour or -1), the menu types, the
 * data component types and the consume effect types (protocol id, name). Run by headless-packets with the server's classpath.
 */
public class GameData {
    public static void main(String[] args) throws IOException {
        SharedConstants.tryDetectVersion();
        Bootstrap.bootStrap();
        try (PrintWriter out = new PrintWriter(Files.newBufferedWriter(Path.of(args[0]), StandardCharsets.UTF_8))) {
            for (int id = 0; id < 256; id++) {
                out.print("color\t" + id + "\t" + MapColor.getColorFromPackedId(id) + "\n");
            }
            for (Item item : BuiltInRegistries.ITEM) {
                int color = item instanceof BlockItem block ? block.getBlock().defaultMapColor().col : -1;
                out.print("item\t" + BuiltInRegistries.ITEM.getId(item) + "\t" + BuiltInRegistries.ITEM.getKey(item).getPath() + "\t" + item.getDescriptionId() + "\t" + color + "\n");
            }
            names(out, "menu", BuiltInRegistries.MENU);
            names(out, "component", BuiltInRegistries.DATA_COMPONENT_TYPE);
            names(out, "consume_effect", BuiltInRegistries.CONSUME_EFFECT_TYPE);
        }
    }

    private static <T> void names(PrintWriter out, String kind, Registry<T> registry) {
        for (T entry : registry) {
            out.print(kind + "\t" + registry.getId(entry) + "\t" + registry.getKey(entry).getPath() + "\n");
        }
    }
}
