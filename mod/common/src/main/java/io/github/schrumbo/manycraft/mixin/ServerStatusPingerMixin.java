package io.github.schrumbo.manycraft.mixin;

import io.github.schrumbo.manycraft.CompanionHooks;
import net.minecraft.client.multiplayer.ServerStatusPinger;
import org.spongepowered.asm.mixin.Mixin;
import org.spongepowered.asm.mixin.injection.At;
import org.spongepowered.asm.mixin.injection.Inject;
import org.spongepowered.asm.mixin.injection.callback.CallbackInfo;

@Mixin(ServerStatusPinger.class)
abstract class ServerStatusPingerMixin {
	@Inject(method = "pingLegacyServer", at = @At("HEAD"), cancellable = true)
	private void manycraft$noDirectLegacyPing(CallbackInfo ci) {
		if (CompanionHooks.proxyRequested()) {
			ci.cancel();
		}
	}
}
