package io.github.schrumbo.manycraft.agent;

import static java.lang.constant.ConstantDescs.CD_Object;
import static java.lang.constant.ConstantDescs.CD_String;
import static java.lang.constant.ConstantDescs.CD_boolean;
import static java.lang.constant.ConstantDescs.CD_int;
import static java.lang.constant.ConstantDescs.CD_long;
import static java.lang.constant.ConstantDescs.CD_void;

import java.lang.classfile.AccessFlags;
import java.lang.classfile.ClassFile;
import java.lang.classfile.ClassHierarchyResolver;
import java.lang.classfile.ClassModel;
import java.lang.classfile.ClassTransform;
import java.lang.classfile.CodeBuilder;
import java.lang.classfile.CodeElement;
import java.lang.classfile.CodeTransform;
import java.lang.classfile.FieldElement;
import java.lang.classfile.FieldModel;
import java.lang.classfile.Label;
import java.lang.classfile.MethodModel;
import java.lang.classfile.MethodTransform;
import java.lang.classfile.Opcode;
import java.lang.classfile.instruction.InvokeInstruction;
import java.lang.classfile.instruction.ReturnInstruction;
import java.lang.constant.ClassDesc;
import java.lang.constant.MethodTypeDesc;
import java.lang.instrument.ClassFileTransformer;
import java.lang.instrument.Instrumentation;
import java.io.ByteArrayInputStream;
import java.io.File;
import java.io.IOException;
import java.io.InputStream;
import java.net.URI;
import java.net.URL;
import java.security.ProtectionDomain;
import java.util.ArrayList;
import java.util.HashMap;
import java.util.HashSet;
import java.util.List;
import java.util.Map;
import java.util.Set;
import java.util.concurrent.atomic.AtomicInteger;
import java.util.function.Consumer;
import java.util.jar.JarEntry;
import java.util.jar.JarFile;

/**
 * Rewrites the game classes the companion hooks into. Every hook checks that its method and call site exist; a missing
 * one is logged once, recorded in {@link Patches} and leaves that class as it was.
 */
final class Patcher implements ClassFileTransformer {
	private static final ClassDesc HOOKS = ClassDesc.of("io.github.schrumbo.manycraft.agent.Hooks");
	private static final ClassDesc PROXY = ClassDesc.of("java.net.Proxy");
	private static final ClassDesc OPTIONAL = ClassDesc.of("java.util.Optional");
	private static final ClassDesc HTTP_CLIENT = ClassDesc.of("java.net.http.HttpClient");
	static final List<String> REALMS = List.of("com/mojang/realmsclient/client/FileDownload", "com/mojang/realmsclient/client/FileUpload");
	private static final ClassDesc GLFW = ClassDesc.of("org.lwjgl.glfw.GLFW");
	private static final int GLFW_VISIBLE = 0x20004;

	private final Instrumentation inst;
	private final Map<String, List<Hook>> hooks;

	Patcher(Instrumentation inst) {
		this(inst, Proxies.requested(), Hooks.headless());
	}

	Patcher(Instrumentation inst, boolean proxy, boolean headless) {
		this.inst = inst;
		this.hooks = table(proxy, headless);
	}

	/**
	 * One change to one method: {@code method} with {@code descriptor} (null matches any, a trailing {@code *} matches the
	 * rest, so a hook survives parameters added or dropped after the ones it reads) gets {@code edit}, which must
	 * call {@code hit} at least once or the hook counts as missing. An {@code alternative} hook stands in for another
	 * version's target and only counts when it hits; alternatives for one feature in one class count as missing only
	 * when none of them hits.
	 */
	record Hook(String feature, String method, String descriptor, Edit edit, boolean alternative) {
		Hook(String feature, String method, String descriptor, Edit edit) {
			this(feature, method, descriptor, edit, false);
		}

		boolean matches(MethodModel m) {
			if (method != null && !m.methodName().equalsString(method)) {
				return false;
			}
			if (descriptor == null) {
				return true;
			}
			return descriptor.endsWith("*") ? m.methodType().stringValue().startsWith(descriptor.substring(0, descriptor.length() - 1)) : m.methodType().equalsString(descriptor);
		}
	}

	interface Edit {
		CodeTransform transform(Runnable hit);
	}

	static Map<String, List<Hook>> table(boolean proxy) {
		return table(proxy, false);
	}

	/**
	 * With {@code headless}, the frame hook may also skip the whole frame of a hidden game.
	 */
	static Map<String, List<Hook>> table(boolean proxy, boolean headless) {
		Map<String, List<Hook>> t = new HashMap<>();
		add(t, "net/minecraft/client/Minecraft",
			new Hook("tick", "tick", "()V", tail(b -> b.aload(0).invokestatic(HOOKS, "tick", type(CD_void, CD_Object)))),
			headless
				? new Hook("frame", "renderFrame", "(Z)V", cancel(b -> b.aload(0).invokestatic(HOOKS, "frameOrSkip", type(CD_boolean, CD_Object))))
				: new Hook("frame", "renderFrame", "(Z)V", head(b -> b.aload(0).invokestatic(HOOKS, "frame", type(CD_void, CD_Object)))),
			new Hook("title", "createTitle", "()Ljava/lang/String;", head(b -> {
				Label original = b.newLabel();
				b.invokestatic(HOOKS, "title", type(CD_String)).dup().ifnull(original).areturn().labelBinding(original);
				b.pop();
			})),
			new Hook("pause", "pauseIfInactive", "()V", cancel(b -> b.invokestatic(HOOKS, "managed", type(CD_boolean)))));
		add(t, "net/minecraft/client/renderer/GameRenderer",
			new Hook("render", "render", null, Patcher::guardRender));
		add(t, "com/mojang/blaze3d/platform/Window",
			new Hook("hidden", null, null, Patcher::hideWindow),
			new Hook("close", "requestClose", "()V", head(b -> b.invokestatic(HOOKS, "closeRequested", type(CD_void))), true),
			new Hook("close", "shouldClose", "()Z", Patcher::watchClose, true));
		add(t, "com/mojang/blaze3d/platform/FramerateLimitTracker",
			new Hook("fps", "getFramerateLimit", "()I", head(b -> {
				Label original = b.newLabel();
				b.invokestatic(HOOKS, "fps", type(CD_int)).dup().ifle(original).ireturn().labelBinding(original);
				b.pop();
			})));
		add(t, "net/minecraft/client/Options",
			new Hook("options", "save", "()V", cancel(b -> b.aload(0).invokestatic(HOOKS, "save", type(CD_boolean, CD_Object)))));
		add(t, "net/minecraft/client/KeyboardHandler",
			new Hook("keys", "keyPress", "(JILnet/minecraft/client/input/KeyEvent;)V", cancel(b -> b.iload(3).aload(4).invokestatic(HOOKS, "key", type(CD_boolean, CD_int, CD_Object)))));
		add(t, "com/mojang/blaze3d/audio/Library",
			new Hook("audio", "cleanup", "()V", head(b -> b.invokestatic(HOOKS, "audioClosed", type(CD_void)))));
		add(t, "net/minecraft/client/gui/screens/multiplayer/JoinMultiplayerScreen",
			new Hook("servers", "init", "()V", tail(b -> b.aload(0).invokestatic(HOOKS, "multiplayerOpened", type(CD_void, CD_Object)))));
		add(t, "net/minecraft/client/multiplayer/ServerList",
			new Hook("servers", "save", "()V", tail(b -> b.invokestatic(HOOKS, "serversSaved", type(CD_void)))));
		add(t, "net/minecraft/client/gui/screens/ConnectScreen",
			new Hook("connecting", "startConnecting", "(Lnet/minecraft/client/gui/screens/Screen;Lnet/minecraft/client/Minecraft;Lnet/minecraft/client/multiplayer/resolver/ServerAddress;Lnet/minecraft/client/multiplayer/ServerData;*",
				head(b -> b.aload(3).invokestatic(HOOKS, "connecting", type(CD_void, CD_Object)))));
		add(t, "net/minecraft/client/multiplayer/chat/ChatListener",
			new Hook("chat", "handleSystemMessage", "(Lnet/minecraft/network/chat/Component;Z*", head(b -> b.aload(1).iload(2).invokestatic(HOOKS, "systemChat", type(CD_void, CD_Object, CD_boolean)))),
			new Hook("chat", "showMessageToPlayer", "(Lnet/minecraft/network/chat/ChatType$Bound;Lnet/minecraft/network/chat/PlayerChatMessage;Lnet/minecraft/network/chat/Component;Lcom/mojang/authlib/GameProfile;ZLjava/time/Instant;)Z",
				head(b -> b.aload(1).aload(2).aload(3).aload(4).aload(6).invokestatic(HOOKS, "playerChat", type(CD_void, CD_Object, CD_Object, CD_Object, CD_Object, CD_Object)))),
			new Hook("disguised", "handleDisguisedChatMessage", "(Lnet/minecraft/network/chat/Component;Lnet/minecraft/network/chat/ChatType$Bound;*",
				head(b -> b.aload(1).aload(2).invokestatic(HOOKS, "disguisedChat", type(CD_void, CD_Object, CD_Object))), true),
			new Hook("chat", "handleOverlay", "(Lnet/minecraft/network/chat/Component;*", head(b -> b.aload(1).invokestatic(HOOKS, "overlayChat", type(CD_void, CD_Object))), true));
		add(t, "net/minecraft/client/gui/Hud",
			new Hook("hud", "extractRenderState", "(Lnet/minecraft/client/gui/GuiGraphicsExtractor;*", tail(b -> b.aload(1).invokestatic(HOOKS, "hud", type(CD_void, CD_Object)))));
		add(t, "net/minecraft/client/gui/Gui",
			new Hook("hud", "extractRenderState", "(Lnet/minecraft/client/gui/GuiGraphicsExtractor;*", tail(b -> b.aload(1).invokestatic(HOOKS, "hud", type(CD_void, CD_Object))), true));
		if (proxy) {
			add(t, "io/netty/bootstrap/Bootstrap",
				new Hook("proxy", "init", "(Lio/netty/channel/Channel;)V", head(b -> b.aload(1).invokestatic(HOOKS, "channel", type(CD_void, CD_Object)))));
			add(t, "net/minecraft/client/main/GameConfig$UserData",
				new Hook("signin", "<init>", null, tail(b -> b.aload(0).aload(0).getfield(ClassDesc.ofInternalName("net/minecraft/client/main/GameConfig$UserData"), "proxy", PROXY)
					.invokestatic(HOOKS, "javaProxy", type(CD_Object, CD_Object)).checkcast(PROXY)
					.putfield(ClassDesc.ofInternalName("net/minecraft/client/main/GameConfig$UserData"), "proxy", PROXY))));
			add(t, "net/minecraft/client/multiplayer/resolver/ServerNameResolver",
				new Hook("dns", "resolveAddress", "(Lnet/minecraft/client/multiplayer/resolver/ServerAddress;)Ljava/util/Optional;", Patcher::remoteResolve));
			for (String realms : REALMS) {
				add(t, realms, new Hook("realms", null, null, Patcher::proxiedHttp));
			}
			add(t, "com/mojang/realmsclient/client/Ping", new Hook("realms", "ping", "(Ljava/lang/String;)I", Patcher::noDirectPing));
		}
		return t;
	}

	private static void add(Map<String, List<Hook>> table, String owner, Hook... hooks) {
		table.computeIfAbsent(owner, k -> new ArrayList<>()).addAll(List.of(hooks));
	}

	@Override
	public byte[] transform(Module module, ClassLoader loader, String name, Class<?> redefined, ProtectionDomain domain, byte[] bytes) {
		List<Hook> list = name == null || redefined != null ? null : hooks.get(name);
		if (list == null) {
			return null;
		}
		try {
			byte[] out = patch(loader, name, bytes, list);
			if (out != null && module != null && module.isNamed() && !module.canRead(Hooks.class.getModule())) {
				inst.redefineModule(module, Set.of(Hooks.class.getModule()), Map.of(), Map.of(), Set.of(), Map.of());
			}
			return out;
		} catch (Throwable e) {
			for (Hook h : list) {
				Patches.record(h.feature(), false);
			}
			Agent.log("cannot patch " + name + ", left as it was: " + e);
			return null;
		} finally {
			Agent.haltIfProxyUnhooked();
		}
	}

	static byte[] patch(ClassLoader loader, String name, byte[] bytes, List<Hook> list) {
		ClassFile cf = files(loader);
		ClassModel model = cf.parse(bytes);
		AtomicInteger[] hits = new AtomicInteger[list.size()];
		for (int i = 0; i < hits.length; i++) {
			hits[i] = new AtomicInteger();
		}
		ClassTransform methods = (b, e) -> {
			if (!(e instanceof MethodModel m) || m.code().isEmpty()) {
				b.with(e);
				return;
			}
			CodeTransform t = null;
			for (int i = 0; i < list.size(); i++) {
				Hook h = list.get(i);
				if (h.matches(m)) {
					AtomicInteger hit = hits[i];
					CodeTransform next = h.edit().transform(hit::incrementAndGet);
					t = t == null ? next : t.andThen(next);
				}
			}
			if (t == null) {
				b.with(e);
			} else {
				b.transformMethod(m, MethodTransform.transformingCode(t));
			}
		};
		ClassTransform transform = name.equals("net/minecraft/client/Options") ? methods.andThen(unfinal("optionsFile")) : methods;
		byte[] out = cf.transformClass(model, transform);
		List<VerifyError> errors = verify(cf, out);
		if (!errors.isEmpty()) {
			for (Hook h : list) {
				Patches.record(h.feature(), false);
			}
			Agent.log("patched " + name + " does not verify, left as it was: " + errors.getFirst().getMessage());
			return null;
		}
		boolean any = false;
		List<String> missing = new ArrayList<>();
		List<String> optional = new ArrayList<>();
		Map<String, Integer> alternatives = new HashMap<>();
		Set<String> alternativeHit = new HashSet<>();
		for (int i = 0; i < list.size(); i++) {
			Hook h = list.get(i);
			if (h.alternative()) {
				alternatives.merge(h.feature(), 1, Integer::sum);
				if (hits[i].get() > 0) {
					alternativeHit.add(h.feature());
				}
			}
		}
		for (int i = 0; i < list.size(); i++) {
			boolean ok = hits[i].get() > 0;
			any |= ok;
			Hook h = list.get(i);
			if (h.alternative() && !ok && (alternatives.get(h.feature()) < 2 || alternativeHit.contains(h.feature()))) {
				if (!alternativeHit.contains(h.feature())) {
					optional.add((h.method() == null ? "*" : h.method()) + " (" + h.feature() + ")");
				}
				continue;
			}
			Patches.record(h.feature(), ok);
			if (!ok) {
				missing.add((h.method() == null ? "*" : h.method()) + " (" + h.feature() + ")");
			}
		}
		if (!optional.isEmpty()) {
			Agent.log("optional hooks not found in " + name + ": " + String.join(", ", optional));
		}
		if (!missing.isEmpty()) {
			Agent.log("hooks not found in " + name + ": " + String.join(", ", missing));
		}
		return any ? out : null;
	}

	private static ClassTransform unfinal(String field) {
		return (b, e) -> {
			if (e instanceof FieldModel f && f.fieldName().equalsString(field)) {
				b.withField(f.fieldName(), f.fieldType(), fb -> {
					for (FieldElement fe : f) {
						if (!(fe instanceof AccessFlags)) {
							fb.with(fe);
						}
					}
					fb.withFlags(f.flags().flagsMask() & ~ClassFile.ACC_FINAL);
				});
			} else {
				b.with(e);
			}
		};
	}

	/**
	 * What is wrong with {@code bytes}, leaving out classes of libraries this loader cannot see.
	 */
	static List<VerifyError> verify(ClassFile cf, byte[] bytes) {
		return cf.verify(bytes).stream().filter(error -> !String.valueOf(error.getMessage()).contains("Could not resolve class")).toList();
	}

	static ClassFile files(ClassLoader loader) {
		ClassLoader own = loader == null ? ClassLoader.getSystemClassLoader() : loader;
		ClassHierarchyResolver resolver = ClassHierarchyResolver.ofResourceParsing(desc -> resource(own, desc))
			.orElse(ClassHierarchyResolver.ofResourceParsing(ClassLoader.getSystemClassLoader()))
			.orElse(ClassHierarchyResolver.defaultResolver())
			.cached();
		return ClassFile.of(ClassFile.ClassHierarchyResolverOption.of(resolver));
	}

	/**
	 * The class file behind {@code desc}, read past jar signatures a loader patched the classes under.
	 */
	static InputStream resource(ClassLoader loader, ClassDesc desc) {
		String name = desc.descriptorString();
		String path = name.substring(1, name.length() - 1) + ".class";
		try (InputStream in = loader.getResourceAsStream(path)) {
			return in == null ? null : new ByteArrayInputStream(in.readAllBytes());
		} catch (SecurityException e) {
			return unsigned(loader.getResource(path));
		} catch (IOException e) {
			return null;
		}
	}

	private static InputStream unsigned(URL url) {
		String s = url == null ? "" : url.toString();
		int bang = s.indexOf("!/");
		if (!s.startsWith("jar:") || bang < 0) {
			return null;
		}
		try (JarFile jar = new JarFile(new File(URI.create(s.substring(4, bang))), false)) {
			JarEntry entry = jar.getJarEntry(s.substring(bang + 2));
			return entry == null ? null : new ByteArrayInputStream(jar.getInputStream(entry).readAllBytes());
		} catch (IOException | IllegalArgumentException e) {
			return null;
		}
	}

	private static MethodTypeDesc type(ClassDesc ret, ClassDesc... params) {
		return MethodTypeDesc.of(ret, params);
	}

	private static boolean invokes(CodeElement e, String owner, String name, String descriptor) {
		return e instanceof InvokeInstruction i && i.owner().asInternalName().equals(owner) && i.name().equalsString(name) && (descriptor == null || i.type().equalsString(descriptor));
	}

	static Edit head(Consumer<CodeBuilder> prologue) {
		return hit -> new CodeTransform() {
			@Override
			public void atStart(CodeBuilder b) {
				prologue.accept(b);
				hit.run();
			}

			@Override
			public void accept(CodeBuilder b, CodeElement e) {
				b.with(e);
			}
		};
	}

	static Edit cancel(Consumer<CodeBuilder> test) {
		return head(b -> {
			Label original = b.newLabel();
			test.accept(b);
			b.ifeq(original).return_().labelBinding(original);
		});
	}

	static Edit tail(Consumer<CodeBuilder> epilogue) {
		return hit -> (b, e) -> {
			if (e instanceof ReturnInstruction r && r.opcode() == Opcode.RETURN) {
				epilogue.accept(b);
				hit.run();
			}
			b.with(e);
		};
	}

	private static CodeTransform guardRender(Runnable hit) {
		CodeTransform skip = head(b -> {
			Label original = b.newLabel();
			b.aload(0).invokestatic(HOOKS, "beforeRender", type(CD_boolean, CD_Object)).ifne(original).return_().labelBinding(original);
		}).transform(hit);
		return skip.andThen(tail(b -> b.aload(0).invokestatic(HOOKS, "afterRender", type(CD_void, CD_Object))).transform(() -> {}));
	}

	private static CodeTransform remoteResolve(Runnable hit) {
		return (b, e) -> {
			if (!invokes(e, "net/minecraft/client/multiplayer/resolver/ServerAddressResolver", "resolve", "(Lnet/minecraft/client/multiplayer/resolver/ServerAddress;)Ljava/util/Optional;")) {
				b.with(e);
				return;
			}
			Label original = b.newLabel();
			Label done = b.newLabel();
			b.dup().invokestatic(HOOKS, "remoteName", type(CD_Object, CD_Object)).dup().ifnull(original);
			b.dup_x2().pop().pop2().checkcast(OPTIONAL).goto_(done);
			b.labelBinding(original).pop();
			b.with(e);
			b.labelBinding(done);
			hit.run();
		};
	}

	private static CodeTransform proxiedHttp(Runnable hit) {
		return (b, e) -> {
			if (invokes(e, "java/net/http/HttpClient$Builder", "build", "()Ljava/net/http/HttpClient;")) {
				b.invokestatic(HOOKS, "httpClient", type(CD_Object, CD_Object)).checkcast(HTTP_CLIENT);
				hit.run();
			} else {
				b.with(e);
			}
		};
	}

	private static CodeTransform noDirectPing(Runnable hit) {
		return (b, e) -> {
			if (invokes(e, "java/net/Socket", "connect", "(Ljava/net/SocketAddress;I)V")) {
				b.invokestatic(HOOKS, "ping", type(CD_void, CD_Object, CD_Object, CD_int));
				hit.run();
			} else {
				b.with(e);
			}
		};
	}

	private static CodeTransform watchClose(Runnable hit) {
		return (b, e) -> {
			b.with(e);
			if (invokes(e, "org/lwjgl/glfw/GLFW", "glfwWindowShouldClose", "(J)Z") || invokes(e, "com/mojang/blaze3d/platform/GLX", "_shouldClose", "(Lcom/mojang/blaze3d/platform/Window;)Z")) {
				b.invokestatic(HOOKS, "closing", type(CD_boolean, CD_boolean));
				hit.run();
			}
		};
	}

	private static CodeTransform hideWindow(Runnable hit) {
		return (b, e) -> {
			if (invokes(e, "com/mojang/renderpearl/api/device/GpuBackend", "createWindow", "(Ljava/lang/String;IIJ)J")) {
				b.invokestatic(HOOKS, "windowFlags", type(CD_long, CD_long));
				hit.run();
			} else if (invokes(e, "org/lwjgl/glfw/GLFW", "glfwCreateWindow", null)) {
				Label shown = b.newLabel();
				b.invokestatic(HOOKS, "startHidden", type(CD_boolean)).ifeq(shown);
				b.ldc(GLFW_VISIBLE).iconst_0().invokestatic(GLFW, "glfwWindowHint", type(CD_void, CD_int, CD_int));
				b.labelBinding(shown);
				hit.run();
			}
			b.with(e);
		};
	}
}
