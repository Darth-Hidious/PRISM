# PRISM Platform
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
**Platform for Research in Intelligent Synthesis of Materials**

A modern, streamlined command-line interface for accessing materials science data from the [OPTIMADE Network](https://www.optimade.org/).

## Overview

PRISM provides a powerful and easy-to-use CLI to search, filter, and retrieve materials data from a vast, federated network of the world's leading materials databases. By leveraging the OPTIMADE standard, PRISM offers a single, unified interface to query dozens of data providers without needing to write custom code for each one.

## Features

- **Unified Search:** Access data from dozens of materials databases (Materials Project, AFLOW, OQMD, etc.) with a single command.
- **Standardized Filtering:** Use the powerful [OPTIMADE filter language](https://www.optimade.org/optimade-python-tools/latest/how_to_guides/filtering_optimade_data/) to query by chemical formula, elements, number of elements, and more.
- **Simple & Fast:** A clean, responsive CLI designed for materials scientists and researchers.
- **Easy Installation:** Get up and running with a single `pip` command.

## Quick Start

The best way to install PRISM is using `pip` within a Python virtual environment.

```bash
# 1. Clone the repository
git clone https://github.com/Darth-Hidious/PRISM.git
cd PRISM

# 2. Create and activate a virtual environment
# On macOS/Linux:
python3 -m venv .venv
source .venv/bin/activate
# On Windows:
# python -m venv .venv
# .venv\Scripts\activate

# 3. Install PRISM in editable mode
pip install -e .

# 4. Verify the installation
prism --help
```

For more detailed instructions, see the [Installation Guide](docs/INSTALL.md).

## Usage

Once installed, you can use the `prism` command to search for materials across the entire OPTIMADE network.

### Basic Search

```bash
# Search for structures containing Silicon and Oxygen
prism search --elements Si O

# Search for structures with a specific chemical formula
prism search --formula "Fe2O3"

# Find all binary compounds containing Cobalt
prism search --elements Co --nelements 2
```

### Advanced Filtering

You can pass any valid OPTIMADE filter string directly to the `--filter` option for more complex queries.

```bash
# Find silicon oxides with 2 or 3 atoms in the unit cell
prism search --filter 'elements HAS ALL "Si", "O" AND natoms<=3'

# Find materials with a specific space group number
prism search --filter 'space_group_number=225'
```

## Development

Contributions are welcome! To set up a development environment:

```bash
# Clone the repository
git clone https://github.com/Darth-Hidious/PRISM.git
cd PRISM

# Create and activate a virtual environment
python3 -m venv .venv
source .venv/bin/activate

# Install in editable mode with development dependencies
pip install -e ".[dev]"

# Run tests
pytest
```

## Contributing

1.  Fork the repository.
2.  Create a feature branch (`git checkout -b feature/my-new-feature`).
3.  Commit your changes (`git commit -am 'Add some feature'`).
4.  Push to the branch (`git push origin feature/my-new-feature`).
5.  Open a Pull Request.

## License

This project is licensed under the MIT License. See the `LICENSE` file for details.
