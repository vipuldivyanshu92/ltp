import asyncio
import os
import datetime
from cryptography import x509
from cryptography.x509.oid import NameOID
from cryptography.hazmat.primitives import hashes
from cryptography.hazmat.primitives.asymmetric import rsa
from cryptography.hazmat.primitives import serialization
from aioquic.asyncio import QuicConnectionProtocol, serve
from aioquic.quic.configuration import QuicConfiguration
from aioquic.quic.events import StreamDataReceived, QuicEvent

CONNECTIONS = set()

def generate_cert():
    if os.path.exists("cert.pem") and os.path.exists("key.pem"):
        return
    key = rsa.generate_private_key(public_exponent=65537, key_size=2048)
    subject = issuer = x509.Name([x509.NameAttribute(NameOID.COMMON_NAME, u"localhost")])
    cert = x509.CertificateBuilder().subject_name(subject).issuer_name(issuer).public_key(key.public_key()).serial_number(x509.random_serial_number()).not_valid_before(datetime.datetime.utcnow()).not_valid_after(datetime.datetime.utcnow() + datetime.timedelta(days=365)).sign(key, hashes.SHA256())
    with open("key.pem", "wb") as f:
        f.write(key.private_bytes(encoding=serialization.Encoding.PEM, format=serialization.PrivateFormat.TraditionalOpenSSL, encryption_algorithm=serialization.NoEncryption()))
    with open("cert.pem", "wb") as f:
        f.write(cert.public_bytes(serialization.Encoding.PEM))

class ReflectorProtocol(QuicConnectionProtocol):
    def connection_made(self, transport):
        super().connection_made(transport)
        CONNECTIONS.add(self)
        print(f"New connection: {len(CONNECTIONS)} active")

    def connection_lost(self, exc):
        CONNECTIONS.discard(self)
        print(f"Connection lost: {len(CONNECTIONS)} active")
        super().connection_lost(exc)

    def quic_event_received(self, event: QuicEvent) -> None:
        if isinstance(event, StreamDataReceived):
            # Broadcast the received chunk to everyone else
            for conn in CONNECTIONS:
                if conn != self:
                    # Send to stream 0 (since local proxies will listen on stream 0 or use the first available bidirectional stream)
                    # We will use the same stream_id it was received on for simplicity, assuming proxies all use stream 0
                    try:
                        conn._quic.send_stream_data(event.stream_id, event.data, event.end_stream)
                        conn.transmit()
                    except Exception as e:
                        pass
        super().quic_event_received(event)

async def main():
    generate_cert()
    configuration = QuicConfiguration(is_client=False, alpn_protocols=["teleop-quic"])
    configuration.load_cert_chain("cert.pem", "key.pem")
    
    port = int(os.environ.get("PORT", 4433))
    print(f"Starting QUIC Reflector on 0.0.0.0:{port}")
    await serve(host="0.0.0.0", port=port, configuration=configuration, create_protocol=ReflectorProtocol)
    await asyncio.Future()

if __name__ == "__main__":
    asyncio.run(main())
