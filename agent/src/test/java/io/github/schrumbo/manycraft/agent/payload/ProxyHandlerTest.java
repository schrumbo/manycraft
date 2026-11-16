package io.github.schrumbo.manycraft.agent.payload;

import static org.junit.jupiter.api.Assertions.assertArrayEquals;
import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertThrows;
import static org.junit.jupiter.api.Assertions.assertTrue;

import io.github.schrumbo.manycraft.agent.ProxySpec;
import io.netty.buffer.ByteBuf;
import io.netty.buffer.Unpooled;
import io.netty.channel.ChannelHandlerContext;
import io.netty.channel.ChannelInboundHandlerAdapter;
import io.netty.channel.embedded.EmbeddedChannel;
import java.io.IOException;
import java.net.InetAddress;
import java.net.InetSocketAddress;
import java.nio.charset.StandardCharsets;
import java.util.ArrayList;
import java.util.List;
import org.junit.jupiter.api.Test;

class ProxyHandlerTest {
	private static final InetSocketAddress PROXY = new InetSocketAddress(InetAddress.getLoopbackAddress(), 1080);

	private static final class Game extends ChannelInboundHandlerAdapter {
		boolean active;
		final List<String> read = new ArrayList<>();
		final List<Throwable> errors = new ArrayList<>();

		@Override
		public void channelActive(ChannelHandlerContext ctx) {
			active = true;
		}

		@Override
		public void channelRead(ChannelHandlerContext ctx, Object msg) {
			ByteBuf buf = (ByteBuf) msg;
			read.add(buf.toString(StandardCharsets.US_ASCII));
			buf.release();
		}

		@Override
		public void exceptionCaught(ChannelHandlerContext ctx, Throwable cause) {
			errors.add(cause);
		}
	}

	private static byte[] out(EmbeddedChannel channel) {
		ByteBuf buf = channel.readOutbound();
		byte[] bytes = new byte[buf.readableBytes()];
		buf.readBytes(bytes);
		buf.release();
		return bytes;
	}

	private static EmbeddedChannel channel(ProxySpec spec, Game game) {
		EmbeddedChannel channel = new EmbeddedChannel();
		channel.pipeline().addLast(ProxyInstaller.HANDLER_NAME, new ProxyHandler(spec, PROXY, 60000));
		channel.pipeline().addLast(game);
		channel.connect(InetSocketAddress.createUnresolved("mc.example.org", 25565));
		return channel;
	}

	@Test
	void socksTunnelsByNameAndHoldsGameTrafficUntilReady() {
		Game game = new Game();
		EmbeddedChannel channel = channel(new ProxySpec(ProxySpec.Kind.SOCKS5, "127.0.0.1", 1080, "u", "p"), game);
		channel.pipeline().fireChannelActive();
		assertArrayEquals(new byte[] {5, 2, 0, 2}, out(channel));
		assertFalse(game.active);
		channel.writeAndFlush(Unpooled.copiedBuffer("hello", StandardCharsets.US_ASCII));
		channel.writeInbound(Unpooled.wrappedBuffer(new byte[] {5, 2}));
		assertArrayEquals(new byte[] {1, 1, 'u', 1, 'p'}, out(channel));
		channel.writeInbound(Unpooled.wrappedBuffer(new byte[] {1, 0}));
		byte[] connect = out(channel);
		assertEquals(3, connect[3]);
		assertEquals("mc.example.org", new String(connect, 5, connect[4], StandardCharsets.US_ASCII));
		assertEquals(25565, ((connect[connect.length - 2] & 0xff) << 8) | (connect[connect.length - 1] & 0xff));
		channel.writeInbound(Unpooled.wrappedBuffer(new byte[] {5, 0, 0, 1, 0, 0, 0, 0, 0, 0, 'x'}));
		assertTrue(game.active);
		assertEquals("hello", new String(out(channel), StandardCharsets.US_ASCII));
		assertEquals(List.of("x"), game.read);
		assertTrue(channel.pipeline().get(ProxyInstaller.HANDLER_NAME) == null);
	}

	@Test
	void aRefusingProxyClosesInsteadOfGoingDirect() {
		Game game = new Game();
		EmbeddedChannel channel = channel(new ProxySpec(ProxySpec.Kind.SOCKS5, "127.0.0.1", 1080, null, null), game);
		channel.pipeline().fireChannelActive();
		assertArrayEquals(new byte[] {5, 1, 0}, out(channel));
		channel.writeInbound(Unpooled.wrappedBuffer(new byte[] {5, 0}));
		out(channel);
		channel.writeInbound(Unpooled.wrappedBuffer(new byte[] {5, 5, 0, 1, 0, 0, 0, 0, 0, 0}));
		assertFalse(game.active);
		assertFalse(channel.isOpen());
		assertTrue(game.errors.getFirst().getMessage().contains("connection refused"));
	}

	@Test
	void httpConnectSendsCredentialsAndAcceptsOnly200() throws IOException {
		Game game = new Game();
		EmbeddedChannel channel = channel(new ProxySpec(ProxySpec.Kind.HTTP, "127.0.0.1", 1080, "u", "p"), game);
		channel.pipeline().fireChannelActive();
		String request = new String(out(channel), StandardCharsets.ISO_8859_1);
		assertTrue(request.startsWith("CONNECT mc.example.org:25565 HTTP/1.1\r\n"));
		assertTrue(request.contains("Proxy-Authorization: Basic dTpw\r\n"));
		channel.writeInbound(Unpooled.copiedBuffer("HTTP/1.1 200 OK\r\n\r\n", StandardCharsets.ISO_8859_1));
		assertTrue(game.active);
		assertThrows(IOException.class, () -> ProxyHandler.httpReply(Unpooled.copiedBuffer("HTTP/1.1 407 Auth\r\n\r\n", StandardCharsets.ISO_8859_1)));
	}

	@Test
	void literalTargetsUseAddressRecords() throws IOException {
		byte[] v4 = ProxyHandler.socksConnect(new InetSocketAddress(InetAddress.getByName("10.1.2.3"), 1));
		assertArrayEquals(new byte[] {5, 1, 0, 1, 10, 1, 2, 3, 0, 1}, v4);
		byte[] v6 = ProxyHandler.socksConnect(new InetSocketAddress(InetAddress.getByName("::1"), 1));
		assertEquals(4, v6[3]);
		assertEquals(22, v6.length);
	}
}
