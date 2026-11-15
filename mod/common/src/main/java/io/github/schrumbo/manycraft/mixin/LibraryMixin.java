package io.github.schrumbo.manycraft.mixin;

import com.mojang.blaze3d.audio.Library;
import io.github.schrumbo.manycraft.CompanionHooks;
import org.spongepowered.asm.mixin.Mixin;
import org.spongepowered.asm.mixin.injection.At;
import org.spongepowered.asm.mixin.injection.Inject;
import org.spongepowered.asm.mixin.injection.callback.CallbackInfo;

@Mixin(Library.class)
abstract class LibraryMixin {
	@Inject(method = "cleanup", at = @At("HEAD"))
	private void manycraft$audioClosed(CallbackInfo ci) {
		CompanionHooks.audioClosed();
	}
}
