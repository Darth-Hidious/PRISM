# 🚀 MARC27's PRISM Platform

**Platform for Research in Intelligent Synthesis of Materials**  
Advanced Materials Discovery & Database Integration Platform

```
    ███╗   ███╗ █████╗ ██████╗  ██████╗██████╗ ███████╗
    ████╗ ████║██╔══██╗██╔══██╗██╔════╝╚════██╗╚════██║
    ██╔████╔██║███████║██████╔╝██║      █████╔╝    ██╔╝
    ██║╚██╔╝██║██╔══██║██╔══██╗██║     ██╔═══╝    ██╔╝ 
    ██║ ╚═╝ ██║██║  ██║██║  ██║╚██████╗███████╗  ██╗
    ╚═╝     ╚═╝╚═╝  ╚═╝╚═╝  ╚═╝ ╚═════╝╚══════╝ ╚══╝
                                                        
         ██████╗ ██████╗ ██╗███████╗███╗   ███╗
         ██╔══██╗██╔══██╗██║██╔════╝████╗ ████║
         ██████╔╝██████╔╝██║███████╗██╔████╔██║
         ██╔═══╝ ██╔══██╗██║╚════██║██║╚██╔╝██║
         ██║     ██║  ██║██║███████║██║ ╚═╝ ██║
         ╚═╝     ╚═╝  ╚═╝╚═╝╚══════╝╚═╝     ╚═╝
```

A comprehensive materials science platform providing unified access to **2M+ materials** across NOMAD, JARVIS, OQMD, and COD databases through a beautiful command-line interface.

## ⚡ Quick Start

```bash
# One-line installation
pip install git+https://github.com/Darth-Hidious/PRISM.git

# Launch MARC27's PRISM
prism

# Start searching materials
prism search --database oqmd --elements Li,O --limit 10
```

## ✨ Features

- 🎨 **Beautiful CLI** with MARC27's custom branding
- 🔍 **Multi-Database Search** across NOMAD, JARVIS, OQMD, COD  
- 📊 **Rich Data Visualization** and export capabilities
- 🚀 **Interactive Modes** with guided tutorials
- ⚡ **High Performance** with rate limiting and optimization
- 🖥️ **Cross-Platform** (Windows, macOS, Linux)

## 📦 Installation

### Quick Install
```bash
# From GitHub
pip install git+https://github.com/Darth-Hidious/PRISM.git

# Or clone and install
git clone https://github.com/Darth-Hidious/PRISM.git
cd PRISM
python quick_install.py
```

### Platform-Specific
```bash
# Linux/macOS
./install.sh

# Windows
install_windows.bat
# or
.\install_windows.ps1
```

📖 **Full installation guide**: [docs/INSTALL.md](docs/INSTALL.md)

## 🚀 Usage

### Interactive Mode
```bash
# Launch with beautiful interface
prism

# Interactive search
prism search --interactive
```

### Command Line
```bash
# Search materials
prism search --database nomad --elements Si,O --formation-energy -2,0

# List databases
prism list-databases

# Export results
prism search --database jarvis --elements Li --export csv
```

### Getting Started
```bash
# Built-in tutorial
prism getting-started

# View examples
prism examples

# Schema documentation
prism schema --command search
```

## 🗃️ Supported Databases

| Database | Materials | Specialization |
|----------|-----------|----------------|
| **NOMAD** | 1.9M+ | DFT calculations, experimental data |
| **JARVIS** | 100K+ | NIST database, 2D materials |
| **OQMD** | 700K+ | Formation energies, stability |
| **COD** | 500K+ | Crystal structures, experimental |

## 🛠️ Development

```bash
# Clone repository
git clone https://github.com/Darth-Hidious/PRISM.git
cd PRISM

# Install development dependencies
pip install -e ".[dev,export,monitoring]"

# Run tests
pytest

# Code formatting
black app/
```

## 📁 Project Structure

```
PRISM/
├── app/
│   ├── cli.py              # Main CLI interface
│   ├── config/
│   │   └── branding.py     # MARC27 branding
│   └── services/
│       └── connectors/     # Database connectors
├── docs/
│   └── INSTALL.md          # Installation guide
├── install_windows.bat     # Windows installer
├── install_windows.ps1     # PowerShell installer
├── quick_install.py        # Cross-platform installer
└── README.md              # This file
```

## 🤝 Contributing

1. Fork the repository
2. Create a feature branch (`git checkout -b feature/amazing-feature`)
3. Commit your changes (`git commit -m 'Add amazing feature'`)
4. Push to the branch (`git push origin feature/amazing-feature`)
5. Open a Pull Request

## 📄 License

This project is licensed under the MIT License - see the [LICENSE](LICENSE) file for details.

## 🆘 Support

- 📧 **Email**: team@marc27.com
- 🐛 **Issues**: [GitHub Issues](https://github.com/Darth-Hidious/PRISM/issues)
- 📖 **Documentation**: Built-in via `prism getting-started`

## 🙏 Acknowledgments

- NOMAD Laboratory for materials data
- NIST JARVIS database
- OQMD and COD databases
- Python community for excellent libraries

---

**MARC27's PRISM Platform - Advancing Materials Science Through Data** 🔬✨
