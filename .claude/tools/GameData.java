import java.io.PrintWriter;
import java.lang.reflect.Field;
import java.lang.reflect.Modifier;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.Collections;
import java.util.zip.ZipEntry;
import java.util.zip.ZipFile;

import net.minecraft.SharedConstants;
import net.minecraft.core.BlockPos;
import net.minecraft.core.Registry;
import net.minecraft.core.registries.BuiltInRegistries;
import net.minecraft.network.syncher.EntityDataAccessor;
import net.minecraft.network.syncher.EntityDataSerializer;
import net.minecraft.network.syncher.EntityDataSerializers;
import net.minecraft.server.Bootstrap;
import net.minecraft.world.item.BlockItem;
import net.minecraft.world.item.Item;
import net.minecraft.world.level.EmptyBlockGetter;
import net.minecraft.world.level.block.Block;
import net.minecraft.world.level.block.RenderShape;
import net.minecraft.world.level.block.state.BlockState;
import net.minecraft.world.level.material.MapColor;

/**
 * Writes what the connection-only client needs from the game code itself, as tab-separated lines: the map colours
 * (packed id, ARGB), the items (protocol id, name, translation key, block map colour or -1), the menu types, the
 * data component types and the consume effect types (protocol id, name), and the block states (protocol id, map colour,
 * kind: 0 air or unseen, 1 full cube, 2 fluid, 3 other solid, 4 no collision), the entity types, the villager types and professions, the entity data serializers
 * (protocol id, field name) and every entity data accessor of the entity classes in the server jar (class, field, index,
 * serializer id). Run by headless-packets with the server's classpath and the path of that jar.
 */
public class GameData {
    public static void main(String[] args) throws Exception {
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
            for (int id = 0; id < Block.BLOCK_STATE_REGISTRY.size(); id++) {
                BlockState state = Block.BLOCK_STATE_REGISTRY.byId(id);
                int kind = kind(state);
                int color = state.getMapColor(EmptyBlockGetter.INSTANCE, BlockPos.ZERO).id;
                out.print("block\t" + id + "\t" + (kind == 2 && color == 0 ? MapColor.WATER.id : color) + "\t" + kind + "\n");
            }
            names(out, "menu", BuiltInRegistries.MENU);
            names(out, "component", BuiltInRegistries.DATA_COMPONENT_TYPE);
            names(out, "consume_effect", BuiltInRegistries.CONSUME_EFFECT_TYPE);
            names(out, "entity", BuiltInRegistries.ENTITY_TYPE);
            names(out, "villager_type", BuiltInRegistries.VILLAGER_TYPE);
            names(out, "villager_profession", BuiltInRegistries.VILLAGER_PROFESSION);
            for (Field f : EntityDataSerializers.class.getDeclaredFields()) {
                if (Modifier.isStatic(f.getModifiers()) && EntityDataSerializer.class.isAssignableFrom(f.getType())) {
                    f.setAccessible(true);
                    out.print("serializer\t" + EntityDataSerializers.getSerializedId((EntityDataSerializer<?>) f.get(null)) + "\t" + f.getName() + "\n");
                }
            }
            try (ZipFile jar = new ZipFile(args[1])) {
                for (ZipEntry entry : Collections.list(jar.entries())) {
                    String name = entry.getName();
                    if (!name.startsWith("net/minecraft/world/entity/") || !name.endsWith(".class")) continue;
                    Class<?> type;
                    try {
                        type = Class.forName(name.substring(0, name.length() - 6).replace('/', '.'), true, GameData.class.getClassLoader());
                    } catch (Throwable e) {
                        continue;
                    }
                    for (Field f : type.getDeclaredFields()) {
                        if (!Modifier.isStatic(f.getModifiers()) || !EntityDataAccessor.class.isAssignableFrom(f.getType())) continue;
                        f.setAccessible(true);
                        EntityDataAccessor<?> accessor = (EntityDataAccessor<?>) f.get(null);
                        out.print("accessor\t" + type.getName().substring(type.getPackageName().length() + 1) + "\t" + f.getName() + "\t" + accessor.id() + "\t" + EntityDataSerializers.getSerializedId(accessor.serializer()) + "\n");
                    }
                }
            }
        }
    }

    private static int kind(BlockState state) {
        boolean fluid = !state.getFluidState().isEmpty();
        boolean open = state.getCollisionShape(EmptyBlockGetter.INSTANCE, BlockPos.ZERO).isEmpty();
        if ((state.getRenderShape() == RenderShape.INVISIBLE || state.getMapColor(EmptyBlockGetter.INSTANCE, BlockPos.ZERO).id == 0) && !fluid) return 0;
        if (fluid && open) return 2;
        if (open) return 4;
        return state.isSolidRender() ? 1 : 3;
    }

    private static <T> void names(PrintWriter out, String kind, Registry<T> registry) {
        for (T entry : registry) {
            out.print(kind + "\t" + registry.getId(entry) + "\t" + registry.getKey(entry).getPath() + "\n");
        }
    }
}