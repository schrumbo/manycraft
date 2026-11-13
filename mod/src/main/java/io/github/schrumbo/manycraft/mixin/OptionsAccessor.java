package io.github.schrumbo.manycraft.mixin;

import net.minecraft.client.Options;
import org.spongepowered.asm.mixin.Mixin;
import org.spongepowered.asm.mixin.gen.Accessor;

@Mixin(Options.class)
public interface OptionsAccessor {
	@Accessor("isApplyingGraphicsPreset")
	void manycraft$setApplyingGraphicsPreset(boolean value);
}
