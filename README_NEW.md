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
   .venv\Scripts\activate  # Windows
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
- **Save Results** - run `prism advanced init` to enable local data persistence

## 🔬 Advanced Features

- **Adaptive Filter Generation**: AI learns from API errors to improve query accuracy
- **Token Optimization**: Smart conversation summarization for efficient API usage
- **Error Recovery**: Multiple fallback strategies for robust operation
- **Database Integration**: Save and analyze results in local SQLite database
- **Extensible Architecture**: Ready for future LLM providers and databases

Ready to revolutionize your materials research? Start with `prism` and let AI guide your discovery journey!