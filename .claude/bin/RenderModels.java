import java.io.PrintStream;
import java.lang.reflect.Field;
import java.nio.charset.StandardCharsets;
import java.util.Map;
import java.util.TreeMap;

import net.minecraft.client.model.geom.LayerDefinitions;
import net.minecraft.client.model.geom.ModelLayerLocation;
import net.minecraft.client.model.geom.ModelPart;
import net.minecraft.client.model.geom.builders.LayerDefinition;

/** Prints every entity model layer of the client on the class path as JSON: parts with their rest pose and the baked polygons of their cubes. */
public class RenderModels {
    public static void main(String[] args) throws Exception {
        net.minecraft.SharedConstants.tryDetectVersion();
        net.minecraft.server.Bootstrap.bootStrap();
        PrintStream out = new PrintStream(System.out, false, StandardCharsets.UTF_8);
        Map<String, LayerDefinition> roots = new TreeMap<>();
        for (Map.Entry<ModelLayerLocation, LayerDefinition> e : LayerDefinitions.createRoots().entrySet()) {
            roots.put(e.getKey().model() + "#" + e.getKey().layer(), e.getValue());
        }
        Field cubes = ModelPart.class.getDeclaredField("cubes");
        Field children = ModelPart.class.getDeclaredField("children");
        cubes.setAccessible(true);
        children.setAccessible(true);
        out.print("{");
        boolean first = true;
        for (Map.Entry<String, LayerDefinition> e : roots.entrySet()) {
            ModelPart root;
            try {
                root = e.getValue().bakeRoot();
            } catch (Throwable t) {
                System.err.println("skip " + e.getKey() + ": " + t);
                continue;
            }
            out.print((first ? "" : ",") + "\n\"" + e.getKey() + "\":");
            first = false;
            part(out, root, cubes, children);
        }
        out.println("}");
        out.flush();
    }

    @SuppressWarnings("unchecked")
    static void part(PrintStream out, ModelPart p, Field cubes, Field children) throws Exception {
        out.print("{\"pose\":[" + p.x + "," + p.y + "," + p.z + "," + p.xRot + "," + p.yRot + "," + p.zRot + "," + p.xScale + "," + p.yScale + "," + p.zScale + "],\"visible\":" + p.visible + ",\"polys\":[");
        boolean first = true;
        for (ModelPart.Cube c : (java.util.List<ModelPart.Cube>) cubes.get(p)) {
            for (ModelPart.Polygon poly : c.polygons) {
                out.print((first ? "" : ",") + "[");
                first = false;
                ModelPart.Vertex[] v = poly.vertices();
                for (int i = 0; i < v.length; i++) {
                    out.print((i == 0 ? "" : ",") + v[i].x() + "," + v[i].y() + "," + v[i].z() + "," + v[i].u() + "," + v[i].v());
                }
                out.print("]");
            }
        }
        out.print("],\"children\":{");
        first = true;
        for (Map.Entry<String, ModelPart> c : ((Map<String, ModelPart>) children.get(p)).entrySet()) {
            out.print((first ? "" : ",") + "\"" + c.getKey() + "\":");
            first = false;
            part(out, c.getValue(), cubes, children);
        }
        out.print("}}");
    }
}
