package io.github.schrumbo.manycraft.mixin;

import com.llamalad7.mixinextras.injector.wrapoperation.Operation;
import com.llamalad7.mixinextras.injector.wrapoperation.WrapOperation;
import io.github.schrumbo.manycraft.CompanionHooks;
import java.util.Optional;
import net.minecraft.client.multiplayer.resolver.ResolvedServerAddress;
import net.minecraft.client.multiplayer.resolver.ServerAddress;
import net.minecraft.client.multiplayer.resolver.ServerAddressResolver;
import net.minecraft.client.multiplayer.resolver.ServerNameResolver;
import org.spongepowered.asm.mixin.Mixin;
import org.spongepowered.asm.mixin.injection.At;

@Mixin(ServerNameResolver.class)
abstract class ServerNameResolverMixin {
	@WrapOperation(
		method = "resolveAddress",
		at = @At(
			value = "INVOKE",
			target = "Lnet/minecraft/client/multiplayer/resolver/ServerAddressResolver;resolve(Lnet/minecraft/client/multiplayer/resolver/ServerAddress;)Ljava/util/Optional;"
		)
	)
	private Optional<ResolvedServerAddress> manycraft$remoteDns(
		ServerAddressResolver resolver,
		ServerAddress address,
		Operation<Optional<ResolvedServerAddress>> original
	) {
		return CompanionHooks.remoteName(address.getHost(), address.getPort())
			.map(ResolvedServerAddress::from)
			.or(() -> original.call(resolver, address));
	}
}
