package io.github.schrumbo.manycraft.agent.payload;

import java.io.IOException;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.NoSuchFileException;
import java.nio.file.Path;
import java.nio.file.StandardCopyOption;
import java.util.ArrayList;
import java.util.HashSet;
import java.util.Iterator;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;
import java.util.Objects;
import java.util.Set;
import java.util.function.BiConsumer;
import java.util.function.Function;

final class OptionsFile {
	private OptionsFile() {}

	static Map<String, String> parse(List<String> lines) {
		Map<String, String> values = new LinkedHashMap<>();
		for (String line : lines) {
			int colon = line.indexOf(':');
			if (colon > 0) {
				values.put(line.substring(0, colon), line.substring(colon + 1));
			}
		}
		return values;
	}

	static Map<String, String> read(Path path) {
		try {
			return parse(Files.readAllLines(path, StandardCharsets.UTF_8));
		} catch (IOException | RuntimeException e) {
			return Map.of();
		}
	}

	static Map<String, String> changed(Map<String, String> before, Map<String, String> after) {
		Map<String, String> changed = new LinkedHashMap<>();
		after.forEach((key, value) -> {
			if (before.containsKey(key) && !Objects.equals(before.get(key), value)) {
				changed.put(key, value);
			}
		});
		return changed;
	}

	static Map<String, String> pending(Map<String, String> current, Map<String, String> values) {
		Map<String, String> pending = new LinkedHashMap<>();
		values.forEach((key, value) -> {
			if (valid(key, value) && !value.equals(current.get(key))) {
				pending.put(key, value);
			}
		});
		return pending;
	}

	static <T> List<T> reconcile(List<T> live, List<T> fresh, Function<T, String> key, BiConsumer<T, T> update) {
		List<T> pool = new ArrayList<>(live);
		List<T> out = new ArrayList<>(fresh.size());
		for (T entry : fresh) {
			T kept = null;
			for (Iterator<T> it = pool.iterator(); it.hasNext();) {
				T candidate = it.next();
				if (Objects.equals(key.apply(candidate), key.apply(entry))) {
					kept = candidate;
					it.remove();
					break;
				}
			}
			if (kept == null) {
				out.add(entry);
			} else {
				update.accept(kept, entry);
				out.add(kept);
			}
		}
		return out;
	}

	static String merge(List<String> lines, Map<String, String> changes) {
		Set<String> seen = new HashSet<>();
		List<String> out = new ArrayList<>(lines.size() + changes.size());
		for (String line : lines) {
			int colon = line.indexOf(':');
			String key = colon > 0 ? line.substring(0, colon) : null;
			if (key != null && changes.containsKey(key)) {
				seen.add(key);
				out.add(key + ":" + changes.get(key));
			} else {
				out.add(line);
			}
		}
		changes.forEach((key, value) -> {
			if (!seen.contains(key)) {
				out.add(key + ":" + value);
			}
		});
		return String.join("\n", out) + "\n";
	}

	static String lines(String version, Map<String, String> values) {
		StringBuilder text = new StringBuilder();
		if (version != null) {
			text.append("version:").append(version).append('\n');
		}
		values.forEach((key, value) -> text.append(key).append(':').append(value).append('\n'));
		return text.toString();
	}

	static boolean valid(String key, String value) {
		return key != null && value != null && !key.isEmpty() && key.indexOf(':') < 0 && key.indexOf('\n') < 0 && key.indexOf('\r') < 0
			&& value.indexOf('\n') < 0 && value.indexOf('\r') < 0;
	}

	static void apply(Path path, Map<String, String> changes) throws IOException {
		List<String> lines;
		try {
			lines = Files.readAllLines(path, StandardCharsets.UTF_8);
		} catch (NoSuchFileException e) {
			lines = List.of();
		}
		write(path, merge(lines, changes));
	}

	static void write(Path path, String text) throws IOException {
		Path tmp = path.resolveSibling(path.getFileName() + ".manycraft.tmp");
		Files.writeString(tmp, text, StandardCharsets.UTF_8);
		Files.move(tmp, path, StandardCopyOption.REPLACE_EXISTING, StandardCopyOption.ATOMIC_MOVE);
	}
}
