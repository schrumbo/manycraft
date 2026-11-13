package io.github.schrumbo.manycraft;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertThrows;
import static org.junit.jupiter.api.Assertions.assertTrue;

import org.junit.jupiter.api.Test;

class KeyChordTest {
	@Test
	void parsesModifiersAndKey() {
		assertEquals(new KeyChord(9, true, false, false, false), KeyChord.parse("ctrl+tab"));
		assertEquals(new KeyChord(9, true, true, false, false), KeyChord.parse("Ctrl+Shift+TAB"));
		assertEquals(new KeyChord(103, true, false, false, false), KeyChord.parse("ctrl+g"));
		assertEquals(new KeyChord(1073741882, false, false, true, false), KeyChord.parse("alt+f1"));
		assertEquals(new KeyChord(27, false, false, false, true), KeyChord.parse("super+key.keyboard.escape"));
	}

	@Test
	void matchesExactModifiers() {
		KeyChord next = KeyChord.parse("ctrl+tab");
		KeyChord prev = KeyChord.parse("ctrl+shift+tab");
		assertTrue(next.matches(9, 64));
		assertTrue(next.matches(9, 128));
		assertFalse(next.matches(9, 64 | 1));
		assertFalse(next.matches(9, 0));
		assertTrue(prev.matches(9, 64 | 2));
		assertFalse(prev.matches(9, 64));
		assertFalse(next.matches(32, 64));
	}

	@Test
	void rejectsBadSpecs() {
		assertThrows(IllegalArgumentException.class, () -> KeyChord.parse("ctrl+"));
		assertThrows(IllegalArgumentException.class, () -> KeyChord.parse("ctrl"));
		assertThrows(IllegalArgumentException.class, () -> KeyChord.parse("tab+ctrl"));
		assertThrows(IllegalArgumentException.class, () -> KeyChord.parse("ctrl+banana"));
	}

	@Test
	void defaultsAreDistinct() {
		var d = Companion.Hotkeys.DEFAULT;
		assertFalse(d.next().matches(d.prev().keycode(), 64 | 1));
		assertFalse(d.next().equals(d.prev()));
		assertFalse(d.grid().equals(d.nextAttention()));
	}
}
