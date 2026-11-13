package io.github.schrumbo.manycraft.mixin;

import io.github.schrumbo.manycraft.CompanionHooks;
import io.netty.channel.Channel;
import org.spongepowered.asm.mixin.Mixin;
import org.spongepowered.asm.mixin.injection.At;
import org.spongepowered.asm.mixin.injection.Inject;
import org.spongepowered.asm.mixin.injection.callback.CallbackInfo;

@Mixin(targets = "net.minecraft.network.Connection$1")
abstract class ConnectionInitializerMixin {
	@Inject(method = "initChannel(Lio/netty/channel/Channel;)V", at = @At("HEAD"))
	private void manycraft$proxy(Channel channel, CallbackInfo ci) {
		CompanionHooks.applyProxy(channel);
	}
}
