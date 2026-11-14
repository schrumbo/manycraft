package io.github.schrumbo.manycraft;

import java.util.Locale;
import java.util.Map;

record KeyChord(int keycode, boolean ctrl, boolean shift, boolean alt, boolean meta) {
	static final int MASK_SHIFT = 3;
	static final int MASK_CTRL = 192;
	static final int MASK_ALT = 768;
	static final int MASK_META = 3072;

	private static final Map<String, Integer> NAMES = Map.ofEntries(
		Map.entry("tab", 9),
		Map.entry("space", 32),
		Map.entry("escape", 27),
		Map.entry("esc", 27),
		Map.entry("enter", 13),
		Map.entry("return", 13),
		Map.entry("backspace", 8),
		Map.entry("delete", 127),
		Map.entry("grave", 96),
		Map.entry("grave_accent", 96),
		Map.entry("minus", 45),
		Map.entry("equal", 61),
		Map.entry("comma", 44),
		Map.entry("period", 46),
		Map.entry("slash", 47),
		Map.entry("semicolon", 59),
		Map.entry("apostrophe", 39),
		Map.entry("left_bracket", 91),
		Map.entry("right_bracket", 93),
		Map.entry("backslash", 92),
		Map.entry("right", 1073741903),
		Map.entry("left", 1073741904),
		Map.entry("down", 1073741905),
		Map.entry("up", 1073741906),
		Map.entry("home", 1073741898),
		Map.entry("page_up", 1073741899),
		Map.entry("end", 1073741901),
		Map.entry("page_down", 1073741902),
		Map.entry("insert", 1073741897)
	);

	static KeyChord parse(String spec) {
		String[] parts = spec.trim().toLowerCase(Locale.ROOT).split("[+]");
		boolean ctrl = false;
		boolean shift = false;
		boolean alt = false;
		boolean meta = false;
		Integer key = null;
		for (int i = 0; i < parts.length; i++) {
			String part = normalize(parts[i]);
			if (part.isEmpty()) {
				throw new IllegalArgumentException("empty key in '" + spec + "'");
			}
			boolean last = i == parts.length - 1;
			switch (part) {
				case "ctrl", "control" -> ctrl = true;
				case "shift" -> shift = true;
				case "alt", "option" -> alt = true;
				case "super", "meta", "cmd", "command", "win" -> meta = true;
				default -> {
					if (!last) {
						throw new IllegalArgumentException("modifier expected in '" + spec + "'");
					}
					key = keycode(part);
				}
			}
		}
		if (key == null) {
			throw new IllegalArgumentException("no key in '" + spec + "'");
		}
		return new KeyChord(key, ctrl, shift, alt, meta);
	}

	boolean matches(int keycode, int modifiers) {
		return keycode == this.keycode
			&& ctrl == ((modifiers & MASK_CTRL) != 0)
			&& shift == ((modifiers & MASK_SHIFT) != 0)
			&& alt == ((modifiers & MASK_ALT) != 0)
			&& meta == ((modifiers & MASK_META) != 0);
	}

	private static String normalize(String part) {
		String p = part.trim();
		for (String prefix : new String[] {"key.keyboard.", "glfw_key_", "key_"}) {
			if (p.startsWith(prefix)) {
				return p.substring(prefix.length());
			}
		}
		return p;
	}

	private static int keycode(String name) {
		Integer named = NAMES.get(name);
		if (named != null) {
			return named;
		}
		if (name.length() == 1) {
			char c = name.charAt(0);
			if ((c >= 'a' && c <= 'z') || (c >= '0' && c <= '9')) {
				return c;
			}
		}
		if (name.length() >= 2 && name.charAt(0) == 'f') {
			try {
				int n = Integer.parseInt(name.substring(1));
				if (n >= 1 && n <= 12) {
					return 1073741882 + n - 1;
				}
			} catch (NumberFormatException ignored) {
			}
		}
		throw new IllegalArgumentException("unknown key '" + name + "'");
	}
}
