package io.github.schrumbo.manycraft.mixin;

import com.mojang.blaze3d.platform.FramerateLimitTracker;
import io.github.schrumbo.manycraft.CompanionHooks;
import org.spongepowered.asm.mixin.Mixin;
import org.spongepowered.asm.mixin.injection.At;
import org.spongepowered.asm.mixin.injection.Inject;
import org.spongepowered.asm.mixin.injection.callback.CallbackInfoReturnable;

@Mixin(FramerateLimitTracker.class)
abstract class FramerateLimitTrackerMixin {
	@Inject(method = "getFramerateLimit", at = @At("HEAD"), cancellable = true)
	private void manycraft$background(CallbackInfoReturnable<Integer> cir) {
		int limit = CompanionHooks.hiddenFramerate();
		if (limit > 0) {
			cir.setReturnValue(limit);
		}
	}
}
