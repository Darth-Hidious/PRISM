"""Docs CLI command group: generate documentation from templates."""
import click
from rich.console import Console


@click.group()
def docs():
    """Commands for generating documentation from templates."""
    pass


README_CONTENT = """
# PRISM: Platform for Research in Intelligent Synthesis of Materials

<p align="center">
    <em>A next-generation command-line interface for materials science research, powered by the OPTIMADE API network and Large Language Models.</em>
</p>

---

PRISM is a powerful, intelligent tool designed to revolutionize materials discovery. It provides a unified interface to query dozens of major materials science databases and leverages cutting-edge AI to make research natural, efficient, and conversational.

## 🌟 Key Features

### **Intelligent Conversational Search**
- **Dynamic Interactive Mode**: PRISM conducts intelligent conversations, asking targeted questions based on your research goals
- **Multi-Step Reasoning**: Enable `--reason` flag for detailed scientific analysis with step-by-step reasoning
- **Adaptive Learning**: The system learns from OPTIMADE API responses to refine filters automatically

### **Unified Database Access**
- **40+ Databases**: Access Materials Project, OQMD, COD, JARVIS, AFLOW, and many more through a single interface
- **OPTIMADE Standard**: Built on the Open Databases Integration for Materials Design specification
- **Smart Provider Selection**: AI automatically selects the best database for your query

### **Multiple LLM Support**
- **Currently Supported**: OpenAI, Google Vertex AI, Anthropic, OpenRouter
- **Coming Soon**: Perplexity, Grok (xAI), Ollama (local models), PRISM Custom Model (trained on materials literature)
- **Quick Switching**: Instantly switch between configured LLM providers

### **Advanced Search Capabilities**
- **Natural Language**: Ask questions like "Materials for space applications with high radiation resistance"
- **Structured Search**: Traditional parameter-based searching with elements, formulas, properties
- **Token-Optimized**: Smart conversation summarization to respect API limits

## 🚀 Core Technologies

- **OPTIMADE**: Industry-standard API for materials database integration
- **MCP (Model Context Protocol)**: Intelligent system translating natural language to database queries
- **Adaptive Filters**: Self-correcting filter generation with error feedback loops
- **BYOK (Bring Your Own Key)**: Full control over LLM usage and costs

## 📋 Command Reference

### **Main Commands**

#### `prism` (Interactive Mode)
Start PRISM without arguments for an interactive session:
```bash
prism
# Ask questions, press 's' to switch LLM, or Enter to exit
```

#### `prism ask` - Intelligent Natural Language Search
```bash
prism ask "Materials for battery electrodes" [OPTIONS]
```

**Advanced Options:**
- `--interactive`: Dynamic conversational refinement with targeted questions
- `--reason`: Multi-step scientific reasoning and analysis
- `--providers TEXT`: Specific databases to search (cod,mp,oqmd,aflow,jarvis)
- `--debug-filter TEXT`: Developer mode - bypass LLM with direct OPTIMADE filter

**Examples:**
```bash
# Basic natural language query
prism ask "High entropy alloys with titanium"

# Interactive consultation mode
prism ask "Materials for space applications" --interactive

# Multi-step reasoning analysis
prism ask "Why are these materials suitable for batteries?" --reason

# Target specific database
prism ask "Perovskite structures" --providers "mp,cod"
```

#### `prism search` - Structured Parameter Search
```bash
prism search [OPTIONS]
```

**Options:**
- `--elements TEXT`: Elements that must be present ("Si,O,Ti")
- `--formula TEXT`: Exact chemical formula ("SiO2")
- `--nelements INTEGER`: Number of elements (2 for binary compounds)
- `--providers TEXT`: Specific databases to query

**Examples:**
```bash
# Find titanium dioxide polymorphs
prism search --formula "TiO2"

# All ternary compounds with lithium and cobalt
prism search --elements "Li,Co" --nelements 3

# Iron-containing materials from OQMD only
prism search --elements "Fe" --providers "oqmd"
```

### **Provider and Configuration**

#### `prism switch-llm` - Quick LLM Provider Switching
```bash
prism switch-llm
```
- Lists all configured providers with current selection
- Shows upcoming providers (Perplexity, Grok, Ollama, PRISM Custom)
- One-command switching between active providers

#### `prism optimade list-dbs` - Database Discovery
```bash
prism optimade list-dbs
```
- Lists all 40+ available OPTIMADE databases
- Shows provider IDs for use with `--providers` flag
- Real-time database availability status

#### `prism advanced` - System Management
```bash
prism advanced configure  # Set up LLM providers and database
prism advanced init       # Initialize local SQLite database
```

#### `prism docs` - Documentation
```bash
prism docs save-readme   # Generate README.md
prism docs save-install  # Generate INSTALL.md
```

## 🎯 Usage Scenarios

### **Research Discovery**
```bash
# Start broad, get refined through conversation
prism ask "Materials for solar panels" --interactive

Q1: Are you looking for photovoltaic materials, transparent conductors, or protective coatings?
Your answer: Photovoltaic materials with high efficiency

Q2: What type of solar cell technology - silicon, perovskite, or organic?
Your answer: Perovskite and silicon

Q3: Are you interested in single junction or tandem cell materials?
Your answer: Tandem cells
```

### **Property-Based Search**
```bash
# Multi-step reasoning for complex queries
prism ask "Why do these materials have high thermal conductivity?" --reason

Step 1: Understanding the Query
[Analysis of thermal conductivity factors]

Step 2: Data Analysis
[Examination of crystal structures and bonding]

Step 3: Scientific Conclusions
[Materials science principles explaining properties]
```

### **Database-Specific Research**
```bash
# Target materials databases by expertise
prism ask "Experimental crystal structures" --providers "cod"
prism ask "DFT-calculated properties" --providers "mp,oqmd"
prism ask "2D materials" --providers "mcloud,twodmatpedia"
```

## 🔧 LLM Provider Configuration

PRISM supports multiple LLM providers with easy switching:

### **Active Providers**
1. **OpenAI** (`OPENAI_API_KEY`): GPT-4, GPT-3.5-turbo
2. **Google Vertex AI** (`GOOGLE_CLOUD_PROJECT`): Gemini models
3. **Anthropic** (`ANTHROPIC_API_KEY`): Claude models
4. **OpenRouter** (`OPENROUTER_API_KEY`): Access to 200+ models

### **Coming Soon**
5. **Perplexity** (`PERPLEXITY_API_KEY`): Research-focused AI
6. **Grok** (`GROK_API_KEY`): xAI's conversational model
7. **Ollama** (`OLLAMA_HOST`): Local model deployment
8. **PRISM Custom** (`PRISM_CUSTOM_API_KEY`): Materials science-trained model

### **Quick Setup**
```bash
prism advanced configure
# Choose provider → Enter API key → Ready to go!

# Or switch anytime:
prism switch-llm
```

## 🏁 Quick Start

1. **Install** (see `INSTALL.md` for full details):
   ```bash
   git clone <repository-url>
   cd PRISM
   python -m venv .venv
   .venv\\\\Scripts\\\\activate  # Windows
   pip install -e .
   ```

2. **Configure LLM Provider**:
   ```bash
   prism advanced configure
   ```

3. **Start Exploring**:
   ```bash
   prism ask "Materials for quantum computing" --interactive
   ```

## 💡 Pro Tips

- **Use Interactive Mode** for exploratory research with unclear requirements
- **Enable Reasoning** (`--reason`) for detailed scientific analysis
- **Try Quick Switching** - press 's' from main screen to change LLM providers
- **Target Databases** - use `--providers` to search specific repositories

- **Adaptive Filter Generation**: AI learns from API errors to improve query accuracy
- **Token Optimization**: Smart conversation summarization for efficient API usage
- **Error Recovery**: Multiple fallback strategies for robust operation
- **Database Integration**: Save and analyze results in local SQLite database
- **Extensible Architecture**: Ready for future LLM providers and databases

Ready to revolutionize your materials research? Start with `prism` and let AI guide your discovery journey!
"""

INSTALL_CONTENT = """
# PRISM Installation Guide

Complete setup guide for PRISM - Platform for Research in Intelligent Synthesis of Materials

## 🔧 Prerequisites

### **System Requirements**
- **Python**: Version 3.9, 3.10, 3.11, or 3.12 (Python 3.13+ not supported due to dependency constraints)
- **Operating System**: Windows, macOS, or Linux
- **Memory**: 4GB+ RAM recommended for local models (Ollama)
- **Storage**: ~500MB for installation and dependencies

### **Required Tools**
- **Git**: For repository cloning
- **Internet**: For database access and LLM API calls

### **LLM Provider Account** (Choose one or more)
- [OpenAI API](https://platform.openai.com/) - GPT models
- [Google Cloud](https://cloud.google.com/vertex-ai) - Gemini models
- [Anthropic](https://console.anthropic.com/) - Claude models
- [OpenRouter](https://openrouter.ai/) - 200+ models (Recommended for beginners)

## 🚀 Installation Steps

### **Step 1: Clone the Repository**
```bash
git clone <repository-url>
cd PRISM
```

### **Step 2: Create Virtual Environment**
**Highly recommended** to avoid dependency conflicts:

```bash
# Create virtual environment
python -m venv .venv

# Activate (Windows)
.venv\\\\Scripts\\\\activate

# Activate (macOS/Linux)
source .venv/bin/activate
```

### **Step 3: Install PRISM**
Install in editable mode with all dependencies:
```bash
pip install -e .
```

This installs:
- Core PRISM application
- OPTIMADE client for database access
- Rich library for enhanced CLI display
- SQLAlchemy for local database management
- All LLM provider SDKs (OpenAI, Anthropic, etc.)

### **Step 4: Initial Configuration**
Configure your first LLM provider:
```bash
prism advanced configure
```

You'll see:
```
Select your LLM provider:
1. OpenAI
2. Google Vertex AI
3. Anthropic
4. OpenRouter
5. Perplexity (coming soon)
6. Grok (coming soon)
7. Ollama Local (coming soon)
8. PRISM Custom Model (coming soon)

Enter the number of your provider: 4
Enter your OpenRouter API key: [your-key-here]
```

**💡 Recommendation**: Choose **OpenRouter** for the easiest setup - it provides access to 200+ models with a single API key.

### **Step 5: Initialize Database** (Optional)
Enable result saving and analysis:
```bash
prism advanced init
```

This creates a local SQLite database for:
- Storing search results
- Query history
- Performance analytics
- Offline access to previous discoveries

## ✅ Verification

Test your installation:

### **Basic Functionality**
```bash
# Check PRISM status
prism

# List available databases
prism optimade list-dbs

# Test structured search
prism search --elements "Ti,O" --nelements 2
```

### **LLM Integration**
```bash
# Test natural language search
prism ask "Materials containing titanium"

# Test interactive mode
prism ask "Battery materials" --interactive

# Test reasoning mode
prism ask "Why are these good conductors?" --reason
```

### **Quick Switching**
```bash
# Switch LLM providers
prism switch-llm

# Or press 's' from main menu
prism
```

## 🔧 Advanced Configuration

### **Multiple LLM Providers**
Configure multiple providers for different use cases:

1. **Research**: OpenRouter (broad model access)
2. **Production**: OpenAI (reliable, fast)
3. **Privacy**: Ollama (local inference)
4. **Analysis**: Anthropic (detailed reasoning)

### **Environment Variables**
Alternative to interactive configuration:

```bash
# Create app/.env file
echo 'OPENAI_API_KEY="your-key-here"' > app/.env
echo 'DATABASE_URL="sqlite:///prism.db"' >> app/.env
echo 'LLM_MODEL="gpt-4"' >> app/.env
```

### **Custom Models**
Prepare for upcoming providers:
```bash
# Ollama setup (when available)
export OLLAMA_HOST="http://localhost:11434"

# PRISM Custom Model (when available)
export PRISM_CUSTOM_API_KEY="your-research-key"
```

## 🐛 Troubleshooting

### **Common Issues**

#### **1. Import Errors**
```bash
# Solution: Ensure virtual environment is activated
.venv\\\\Scripts\\\\activate  # Windows
source .venv/bin/activate  # macOS/Linux

# Reinstall if needed
pip install -e .
```

#### **2. LLM Connection Failed**
```bash
# Check API key configuration
prism advanced configure

# Test connection
prism switch-llm
```

#### **3. Unicode Errors (Windows)**
- PRISM handles this automatically with fallbacks
- Rich library provides compatible display modes

#### **4. Database Initialization**
```bash
# Reset database if corrupted
rm prism.db
prism advanced init
```

### **Performance Optimization**

#### **Token Management**
- Use `--interactive` for focused conversations
- Enable `--reason` only when detailed analysis is needed
- Target specific `--providers` to reduce noise

#### **Local Caching**
- Save frequently used results with `prism advanced init`
- Results are automatically cached to local database
- Use saved data for offline analysis

## 🔄 Updating PRISM

Keep PRISM up-to-date with the latest features:

```bash
# Pull latest changes
git pull origin main

# Update dependencies
pip install -e .

# Regenerate documentation
prism docs save-readme
prism docs save-install
```

## 🆘 Getting Help

### **Built-in Help**
```bash
prism --help                    # Main commands
prism ask --help               # Natural language search
prism search --help            # Structured search
prism advanced configure --help # Configuration options
```

### **Quick Reference**
```bash
prism                          # Interactive mode
prism ask "query" --interactive # Conversational search
prism search --elements "Fe,Ni" # Direct parameter search
prism switch-llm               # Change LLM provider
prism optimade list-dbs        # Available databases
```

### **Support Resources**
- **Documentation**: Use `prism docs save-readme` for latest features
- **Examples**: Built into help system and main interface
- **Provider Status**: Real-time database availability via `prism optimade list-dbs`

## 🎯 Next Steps

After successful installation:

1. **Explore Interactive Mode**:
   ```bash
   prism ask "Materials for renewable energy" --interactive
   ```

2. **Try Different LLM Providers**:
   ```bash
   prism switch-llm
   ```

3. **Analyze Results with Reasoning**:
   ```bash
   prism ask "Why are perovskites promising for solar cells?" --reason
   ```

4. **Save Important Discoveries**:
   ```bash
   prism advanced init  # Enable database
   # Results automatically saved during searches
   ```

Welcome to the future of materials research! 🚀
"""


@docs.command()
def save_readme():
    """Saves the project README.md file."""
    console = Console(force_terminal=True, width=120)
    with open("README.md", "w") as f:
        f.write(README_CONTENT)
    console.print("[green]SUCCESS:[/green] `README.md` saved successfully.")


@docs.command()
def save_install():
    """Saves the project INSTALL.md file."""
    console = Console(force_terminal=True, width=120)
    with open("INSTALL.md", "w") as f:
        f.write(INSTALL_CONTENT)
    console.print("[green]SUCCESS:[/green] `INSTALL.md` saved successfully.")
