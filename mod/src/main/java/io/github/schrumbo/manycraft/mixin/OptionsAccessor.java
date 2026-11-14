package io.github.schrumbo.manycraft.mixin;

import java.io.File;
import net.minecraft.client.Options;
import net.minecraft.client.PreferredGraphicsApi;
import org.spongepowered.asm.mixin.Mixin;
import org.spongepowered.asm.mixin.Mutable;
import org.spongepowered.asm.mixin.gen.Accessor;

@Mixin(Options.class)
public interface OptionsAccessor {
	@Accessor("isApplyingGraphicsPreset")
	void manycraft$setApplyingGraphicsPreset(boolean value);

	@Mutable
	@Accessor("optionsFile")
	void manycraft$setOptionsFile(File file);

	@Accessor("preferredGraphicsBackendFromStartup")
	PreferredGraphicsApi manycraft$getGraphicsBackendFromStartup();

	@Accessor("preferredGraphicsBackendFromStartup")
	void manycraft$setGraphicsBackendFromStartup(PreferredGraphicsApi value);
}
