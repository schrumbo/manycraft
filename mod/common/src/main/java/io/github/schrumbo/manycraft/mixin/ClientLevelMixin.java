package io.github.schrumbo.manycraft.mixin;

import io.github.schrumbo.manycraft.CompanionHooks;
import net.minecraft.client.multiplayer.ClientLevel;
import org.spongepowered.asm.mixin.Mixin;
import org.spongepowered.asm.mixin.injection.At;
import org.spongepowered.asm.mixin.injection.Inject;
import org.spongepowered.asm.mixin.injection.callback.CallbackInfo;

@Mixin(ClientLevel.class)
abstract class ClientLevelMixin {
	@Inject(method = "animateTick", at = @At("HEAD"), cancellable = true)
	private void manycraft$noAmbienceWhileHidden(int x, int y, int z, CallbackInfo ci) {
		if (CompanionHooks.skipsAmbience()) {
			ci.cancel();
		}
	}
}
