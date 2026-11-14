package io.github.schrumbo.manycraft.mixin;

import io.github.schrumbo.manycraft.CompanionHooks;
import java.net.Proxy;
import net.minecraft.client.User;
import org.spongepowered.asm.mixin.Final;
import org.spongepowered.asm.mixin.Mixin;
import org.spongepowered.asm.mixin.Mutable;
import org.spongepowered.asm.mixin.Shadow;
import org.spongepowered.asm.mixin.injection.At;
import org.spongepowered.asm.mixin.injection.Inject;
import org.spongepowered.asm.mixin.injection.callback.CallbackInfo;

@Mixin(targets = "net.minecraft.client.main.GameConfig$UserData")
abstract class UserDataMixin {
	@Shadow
	@Final
	@Mutable
	public Proxy proxy;

	@Inject(method = "<init>", at = @At("RETURN"))
	private void manycraft$proxy(User user, Proxy proxy, CallbackInfo ci) {
		Proxy ours = CompanionHooks.javaProxy();
		if (ours != null) {
			this.proxy = ours;
		}
	}
}
