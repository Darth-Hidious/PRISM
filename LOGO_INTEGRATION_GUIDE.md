# Company Logo Integration Guide for PRISM Platform

## 🎨 How to Add Your Company Logo

Your PRISM platform is now configured with a flexible branding system. Here's how to integrate your company logo:

### 📁 Files to Modify

1. **`app/config/branding.py`** - Main branding configuration
2. **`app/cli.py`** - Automatically imports from branding config
3. **`install.sh`** - Installation script with logo
4. **`quick_install.py`** - Quick installer with logo

### 🔧 Method 1: Replace ASCII Art Directly

Edit `app/config/branding.py` and replace the `COMPANY_LOGO` variable:

```python
COMPANY_LOGO = """
Your ASCII art here...
"""
```

### 🖼️ Method 2: Convert Image to ASCII Art

**Option A: Send me your logo image**
- Upload PNG, JPG, or SVG file
- I'll convert it to ASCII art
- Best quality and accuracy

**Option B: Use online converters**
1. Go to: https://www.text-image.com/convert/ascii.html
2. Upload your logo image
3. Adjust settings:
   - Width: 60-80 characters
   - Use block characters: ██
4. Copy the ASCII art to `COMPANY_LOGO`

**Option C: Use command-line tools**
```bash
# Install ascii-image-converter
go install github.com/TheZoraiz/ascii-image-converter@latest

# Convert your logo
ascii-image-converter your-logo.png --width 60 --colored=false
```

### 📝 Method 3: Text-Based Logo

For simple text logos, edit these fields in `app/config/branding.py`:

```python
COMPANY_NAME = "Your Company"
COMPANY_TAGLINE = "Your tagline here"
COMPANY_DESCRIPTION = "Your description"

# Create text-based ASCII art
COMPANY_LOGO = """
██╗   ██╗ ██████╗ ██╗   ██╗██████╗ 
╚██╗ ██╔╝██╔═══██╗██║   ██║██╔══██╗
 ╚████╔╝ ██║   ██║██║   ██║██████╔╝
  ╚██╔╝  ██║   ██║██║   ██║██╔══██╗
   ██║   ╚██████╔╝╚██████╔╝██║  ██║
   ╚═╝    ╚═════╝  ╚═════╝ ╚═╝  ╚═╝
   
   ██████╗ ██████╗ ███╗   ███╗██████╗  █████╗ ███╗   ██╗██╗   ██╗
  ██╔════╝██╔═══██╗████╗ ████║██╔══██╗██╔══██╗████╗  ██║╚██╗ ██╔╝
  ██║     ██║   ██║██╔████╔██║██████╔╝███████║██╔██╗ ██║ ╚████╔╝ 
  ██║     ██║   ██║██║╚██╔╝██║██╔═══╝ ██╔══██║██║╚██╗██║  ╚██╔╝  
  ╚██████╗╚██████╔╝██║ ╚═╝ ██║██║     ██║  ██║██║ ╚████║   ██║   
   ╚═════╝ ╚═════╝ ╚═╝     ╚═╝╚═╝     ╚═╝  ╚═╝╚═╝  ╚═══╝   ╚═╝   
"""
```

### 🎨 Method 4: Custom Design Request

**Tell me about your company:**
1. Company name
2. Industry/field
3. Logo style preferences
4. Colors (if any)
5. Any specific symbols or elements

**Example request:**
> "My company is 'Quantum Materials Inc.', we work in quantum computing materials. I'd like a modern, tech-focused logo with the letter 'Q' prominently featured, maybe with some geometric elements."

### 🌈 Color Customization

Update colors in `app/config/branding.py`:

```python
# Color Scheme (Rich library color names)
PRIMARY_COLOR = "cyan"      # Main brand color
SECONDARY_COLOR = "blue"    # Secondary accents  
ACCENT_COLOR = "green"      # Highlights
WARNING_COLOR = "yellow"    # Warnings
ERROR_COLOR = "red"         # Errors
```

Available colors: `red`, `green`, `yellow`, `blue`, `magenta`, `cyan`, `white`, `bright_red`, `bright_green`, etc.

### 📊 Feature List Customization

Update your company's key features:

```python
FEATURE_LIST = [
    "✨ Your key feature 1",
    "🔬 Your key feature 2", 
    "🚀 Your key feature 3",
    "⚡ Your key feature 4",
    "📊 Your key feature 5"
]
```

### 📞 Contact Information

Update company details:

```python
COMPANY_EMAIL = "contact@yourcompany.com"
COMPANY_URL = "https://www.yourcompany.com"
SUPPORT_URL = "https://support.yourcompany.com"
```

## 🚀 Making It "uv install" Ready

To make your PRISM platform installable like modern packages:

### 1. Update Package Information

Edit `pyproject.toml`:
```toml
[project]
name = "your-company-prism"
authors = [{name = "Your Company", email = "dev@yourcompany.com"}]
description = "Your company's materials discovery platform"
```

### 2. Publish to PyPI

```bash
# Build package
python -m build

# Upload to PyPI (requires account)
python -m twine upload dist/*
```

### 3. Users Install With

```bash
# With uv (fastest)
uv add your-company-prism

# With pip
pip install your-company-prism

# From source
uv pip install git+https://github.com/yourcompany/PRISM.git
```

## ✅ Testing Your Branding

After making changes, test with:

```bash
# Test launch screen
python -m app.cli

# Test help system  
python -m app.cli --help

# Test database listing
python -m app.cli list-databases
```

## 📱 Logo Specifications

**For best results:**
- Width: 60-80 characters
- Height: 6-12 lines
- Use Unicode block characters: `█ ▀ ▄ ▌ ▐ ░ ▒ ▓`
- Avoid complex details in ASCII
- Test in terminal to ensure compatibility

## 🎯 Ready to Implement?

**Just tell me:**
1. **How you want to provide your logo** (image file, description, company name)
2. **Your company details** (name, tagline, key features)
3. **Color preferences** (if any)

I'll create the perfect ASCII art logo and integrate it into your PRISM platform!

---

*Current Status: ✅ Branding system configured and ready for your company logo*
