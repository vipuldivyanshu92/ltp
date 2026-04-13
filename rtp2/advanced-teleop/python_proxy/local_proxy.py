import asyncio
import websockets
import argparse
from aioquic.asyncio.client import connect
from aioquic.quic.configuration import QuicConfiguration
from aioquic.quic.events import StreamDataReceived

parser = argparse.ArgumentParser(description="WebSocket to QUIC Proxy")
parser.add_argument("--quic-host", type=str, required=True, help="Host of the QUIC reflector server")
parser.add_argument("--quic-port", type=int, default=4433, help="Port of the QUIC reflector server")
parser.add_argument("--ws-port", type=int, default=8080, help="Local WebSocket port to listen on")
args = parser.parse_args()

class ProxyConnection:
    def __init__(self, quic_protocol, stream_id):
        self.quic_protocol = quic_protocol
        self.stream_id = stream_id
        self.ws = None
        self.buffer = b""

    async def handle_ws(self, websocket):
        self.ws = websocket
        print(f"WebSocket client connected to local proxy")
        try:
            async for message in websocket:
                data = message if isinstance(message, bytes) else message.encode('utf-8')
                # Frame the message to preserve boundaries over QUIC stream
                length = len(data).to_bytes(4, 'big')
                self.quic_protocol._quic.send_stream_data(self.stream_id, length + data)
                self.quic_protocol.transmit()
        except websockets.exceptions.ConnectionClosed:
            print("WebSocket connection closed")
            self.ws = None

    def on_quic_data(self, data):
        self.buffer += data
        while len(self.buffer) >= 4:
            length = int.from_bytes(self.buffer[:4], 'big')
            if len(self.buffer) >= 4 + length:
                msg = self.buffer[4:4+length]
                self.buffer = self.buffer[4+length:]
                if self.ws:
                    asyncio.create_task(self.ws.send(msg))
            else:
                break

async def main():
    config = QuicConfiguration(is_client=True, verify_mode=False, alpn_protocols=["teleop-quic"])
    
    print(f"Connecting to QUIC Reflector at {args.quic_host}:{args.quic_port}...")
    async with connect(args.quic_host, args.quic_port, configuration=config) as quic_protocol:
        print("Connected to reflector.")
        # Open a bidirectional stream
        stream_id = quic_protocol._quic.get_next_available_stream_id()
        proxy_conn = ProxyConnection(quic_protocol, stream_id)

        # Hook into aioquic event loop for this specific connection
        original_event_received = quic_protocol.quic_event_received
        def quic_event_received(event):
            if isinstance(event, StreamDataReceived):
                proxy_conn.on_quic_data(event.data)
            original_event_received(event)
        quic_protocol.quic_event_received = quic_event_received

        # Make sure the stream is open by sending a tiny ping (or just let the first payload open it)
        quic_protocol._quic.send_stream_data(stream_id, b"")
        quic_protocol.transmit()

        async def handler(websocket):
            await proxy_conn.handle_ws(websocket)

        print(f"Starting local WebSocket server on ws://localhost:{args.ws_port}")
        async with websockets.serve(handler, "localhost", args.ws_port):
            # Keep QUIC connection alive
            await asyncio.Future()

if __name__ == "__main__":
    asyncio.run(main())
