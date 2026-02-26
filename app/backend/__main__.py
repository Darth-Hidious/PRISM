"""Entry point: python3 -m app.backend

Starts the JSON-RPC stdio server for the Ink frontend.
"""
from app.backend.server import StdioServer

if __name__ == "__main__":
    server = StdioServer()
    server.run()
