package io.github.schrumbo.manycraft.agent.payload;

import io.github.schrumbo.manycraft.agent.Proxies;
import io.github.schrumbo.manycraft.agent.ProxySpec;
import io.netty.buffer.ByteBuf;
import io.netty.buffer.Unpooled;
import io.netty.channel.ChannelDuplexHandler;
import io.netty.channel.ChannelHandlerContext;
import io.netty.channel.ChannelPromise;
import io.netty.util.ReferenceCountUtil;
import java.io.IOException;
import java.net.InetSocketAddress;
import java.net.SocketAddress;
import java.nio.charset.StandardCharsets;
import java.util.ArrayDeque;
import java.util.Base64;
import java.util.concurrent.ScheduledFuture;
import java.util.concurrent.TimeUnit;

final class ProxyHandler extends ChannelDuplexHandler {
	private enum Step { IDLE, GREETING, AUTH, CONNECT, DONE }

	private record Pending(Object message, ChannelPromise promise) {}

	private final ProxySpec spec;
	private final InetSocketAddress proxy;
	private final int timeoutMs;
	private final ArrayDeque<Pending> pending = new ArrayDeque<>();
	private Step step = Step.IDLE;
	private SocketAddress target;
	private ByteBuf received;
	private ScheduledFuture<?> timeout;
	private boolean flushWanted;

	ProxyHandler(ProxySpec spec, InetSocketAddress proxy, int timeoutMs) {
		this.spec = spec;
		this.proxy = proxy;
		this.timeoutMs = timeoutMs;
	}

	@Override
	public void connect(ChannelHandlerContext ctx, SocketAddress remote, SocketAddress local, ChannelPromise promise) {
		if (!(remote instanceof InetSocketAddress)) {
			ctx.pipeline().remove(this);
			ctx.connect(remote, local, promise);
			return;
		}
		target = Proxies.forProxy(remote);
		ctx.connect(proxy, local, promise);
	}

	@Override
	public void channelActive(ChannelHandlerContext ctx) {
		if (target == null) {
			ctx.pipeline().remove(this);
			ctx.fireChannelActive();
			return;
		}
		timeout = ctx.executor().schedule(() -> fail(ctx, new IOException("no answer in time")), timeoutMs, TimeUnit.MILLISECONDS);
		try {
			if (spec.kind() == ProxySpec.Kind.HTTP) {
				step = Step.CONNECT;
				ctx.writeAndFlush(Unpooled.wrappedBuffer(httpConnect(target, spec)));
			} else {
				step = Step.GREETING;
				ctx.writeAndFlush(Unpooled.wrappedBuffer(spec.hasCredentials() ? new byte[] {5, 2, 0, 2} : new byte[] {5, 1, 0}));
			}
		} catch (RuntimeException e) {
			fail(ctx, e);
		}
	}

	@Override
	public void channelRead(ChannelHandlerContext ctx, Object msg) {
		if (step == Step.DONE || !(msg instanceof ByteBuf in)) {
			ctx.fireChannelRead(msg);
			return;
		}
		if (received == null) {
			received = ctx.alloc().buffer();
		}
		received.writeBytes(in);
		in.release();
		try {
			advance(ctx);
		} catch (IOException | RuntimeException e) {
			fail(ctx, e);
		}
	}

	private void advance(ChannelHandlerContext ctx) throws IOException {
		while (step != Step.DONE) {
			switch (step) {
				case GREETING -> {
					if (received.readableBytes() < 2) {
						return;
					}
					int version = received.readUnsignedByte();
					int method = received.readUnsignedByte();
					if (version != 5 || (method != 0 && method != 2) || (method == 2 && !spec.hasCredentials())) {
						throw new IOException("SOCKS5 sign-in method refused");
					}
					if (method == 2) {
						step = Step.AUTH;
						ctx.writeAndFlush(Unpooled.wrappedBuffer(socksAuth(spec)));
					} else {
						step = Step.CONNECT;
						ctx.writeAndFlush(Unpooled.wrappedBuffer(socksConnect(target)));
					}
				}
				case AUTH -> {
					if (received.readableBytes() < 2) {
						return;
					}
					received.readUnsignedByte();
					if (received.readUnsignedByte() != 0) {
						throw new IOException("SOCKS5 credentials rejected");
					}
					step = Step.CONNECT;
					ctx.writeAndFlush(Unpooled.wrappedBuffer(socksConnect(target)));
				}
				case CONNECT -> {
					boolean done = spec.kind() == ProxySpec.Kind.HTTP ? httpReply(received) : socksReply(received);
					if (!done) {
						return;
					}
					established(ctx);
				}
				default -> {
					return;
				}
			}
		}
	}

	private void established(ChannelHandlerContext ctx) {
		step = Step.DONE;
		if (timeout != null) {
			timeout.cancel(false);
		}
		ByteBuf rest = received != null && received.isReadable() ? received.readRetainedSlice(received.readableBytes()) : null;
		release();
		ctx.fireChannelActive();
		while (!pending.isEmpty()) {
			Pending p = pending.poll();
			ctx.write(p.message(), p.promise());
		}
		if (flushWanted) {
			ctx.flush();
		}
		ctx.pipeline().remove(this);
		if (rest != null) {
			ctx.fireChannelRead(rest);
		}
	}

	@Override
	public void write(ChannelHandlerContext ctx, Object msg, ChannelPromise promise) {
		if (step == Step.DONE) {
			ctx.write(msg, promise);
		} else {
			pending.add(new Pending(msg, promise));
		}
	}

	@Override
	public void flush(ChannelHandlerContext ctx) {
		if (step == Step.DONE) {
			ctx.flush();
		} else {
			flushWanted = true;
		}
	}

	@Override
	public void channelInactive(ChannelHandlerContext ctx) {
		if (step != Step.DONE && target != null) {
			fail(ctx, new IOException("connection closed by the proxy"));
		}
		ctx.fireChannelInactive();
	}

	@Override
	public void exceptionCaught(ChannelHandlerContext ctx, Throwable cause) {
		if (step != Step.DONE && target != null) {
			fail(ctx, cause);
		} else {
			ctx.fireExceptionCaught(cause);
		}
	}

	@Override
	public void handlerRemoved(ChannelHandlerContext ctx) {
		release();
		failPending(new IOException("proxy handler removed"));
	}

	private void fail(ChannelHandlerContext ctx, Throwable cause) {
		if (step == Step.DONE) {
			return;
		}
		step = Step.DONE;
		if (timeout != null) {
			timeout.cancel(false);
		}
		release();
		IOException failure = new IOException("Proxy " + proxy.getHostString() + ":" + proxy.getPort() + ": " + cause.getMessage(), cause);
		failPending(failure);
		ctx.fireExceptionCaught(failure);
		ctx.close();
	}

	private void failPending(Throwable cause) {
		while (!pending.isEmpty()) {
			Pending p = pending.poll();
			ReferenceCountUtil.release(p.message());
			p.promise().tryFailure(cause);
		}
	}

	private void release() {
		if (received != null) {
			received.release();
			received = null;
		}
	}

	static byte[] socksAuth(ProxySpec spec) {
		byte[] user = spec.user().getBytes(StandardCharsets.UTF_8);
		byte[] pass = spec.password().getBytes(StandardCharsets.UTF_8);
		if (user.length > 255 || pass.length > 255) {
			throw new IllegalArgumentException("credentials too long for SOCKS5");
		}
		byte[] out = new byte[3 + user.length + pass.length];
		out[0] = 1;
		out[1] = (byte) user.length;
		System.arraycopy(user, 0, out, 2, user.length);
		out[2 + user.length] = (byte) pass.length;
		System.arraycopy(pass, 0, out, 3 + user.length, pass.length);
		return out;
	}

	static byte[] socksConnect(SocketAddress remote) {
		InetSocketAddress address = (InetSocketAddress) remote;
		byte[] host;
		byte type;
		if (address.isUnresolved()) {
			host = address.getHostString().getBytes(StandardCharsets.US_ASCII);
			if (host.length > 255) {
				throw new IllegalArgumentException("server name too long for SOCKS5");
			}
			type = 3;
		} else {
			host = address.getAddress().getAddress();
			type = (byte) (host.length == 4 ? 1 : 4);
		}
		int nameLength = type == 3 ? 1 : 0;
		byte[] out = new byte[4 + nameLength + host.length + 2];
		out[0] = 5;
		out[1] = 1;
		out[2] = 0;
		out[3] = type;
		if (type == 3) {
			out[4] = (byte) host.length;
		}
		System.arraycopy(host, 0, out, 4 + nameLength, host.length);
		out[out.length - 2] = (byte) (address.getPort() >> 8);
		out[out.length - 1] = (byte) address.getPort();
		return out;
	}

	static boolean socksReply(ByteBuf in) throws IOException {
		if (in.readableBytes() < 5) {
			return false;
		}
		int start = in.readerIndex();
		int type = in.getUnsignedByte(start + 3);
		int length = switch (type) {
			case 1 -> 4;
			case 4 -> 16;
			case 3 -> 1 + in.getUnsignedByte(start + 4);
			default -> throw new IOException("SOCKS5 reply with an unknown address type");
		};
		if (in.readableBytes() < 4 + length + 2) {
			return false;
		}
		int version = in.getUnsignedByte(start);
		int reply = in.getUnsignedByte(start + 1);
		in.skipBytes(4 + length + 2);
		if (version != 5) {
			throw new IOException("not a SOCKS5 proxy");
		}
		if (reply != 0) {
			throw new IOException("SOCKS5 connect failed (" + socksReason(reply) + ")");
		}
		return true;
	}

	private static String socksReason(int reply) {
		return switch (reply) {
			case 2 -> "not allowed";
			case 3 -> "network unreachable";
			case 4 -> "host unreachable";
			case 5 -> "connection refused";
			case 6 -> "TTL expired";
			default -> "code " + reply;
		};
	}

	static byte[] httpConnect(SocketAddress remote, ProxySpec spec) {
		InetSocketAddress address = (InetSocketAddress) remote;
		String host = address.isUnresolved() ? address.getHostString() : address.getAddress().getHostAddress();
		String authority = (host.indexOf(':') >= 0 ? "[" + host + "]" : host) + ":" + address.getPort();
		StringBuilder request = new StringBuilder("CONNECT ").append(authority).append(" HTTP/1.1\r\nHost: ").append(authority).append("\r\n");
		if (spec.hasCredentials()) {
			String token = Base64.getEncoder().encodeToString((spec.user() + ":" + spec.password()).getBytes(StandardCharsets.UTF_8));
			request.append("Proxy-Authorization: Basic ").append(token).append("\r\n");
		}
		return request.append("\r\n").toString().getBytes(StandardCharsets.ISO_8859_1);
	}

	static boolean httpReply(ByteBuf in) throws IOException {
		int end = -1;
		for (int i = in.readerIndex(); i + 3 < in.writerIndex(); i++) {
			if (in.getByte(i) == '\r' && in.getByte(i + 1) == '\n' && in.getByte(i + 2) == '\r' && in.getByte(i + 3) == '\n') {
				end = i + 4;
				break;
			}
		}
		if (end < 0) {
			if (in.readableBytes() > 16384) {
				throw new IOException("oversized HTTP proxy reply");
			}
			return false;
		}
		String head = in.toString(in.readerIndex(), end - in.readerIndex(), StandardCharsets.ISO_8859_1);
		in.readerIndex(end);
		int lineEnd = head.indexOf('\r');
		String status = lineEnd < 0 ? head : head.substring(0, lineEnd);
		String[] parts = status.split(" ", 3);
		if (parts.length < 2 || !parts[0].startsWith("HTTP/1.") || !parts[1].equals("200")) {
			throw new IOException("HTTP proxy refused: " + status);
		}
		return true;
	}
}
