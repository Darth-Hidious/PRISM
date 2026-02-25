"""Allow ``python -m app.cli`` to work."""
from app.cli.main import cli

if __name__ == "__main__":
    cli()
