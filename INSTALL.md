
# Installation Guide

Follow these steps to get PRISM up and running on your system.

## Prerequisites

- **Python**: PRISM requires Python version 3.9, 3.10, 3.11, or 3.12. It is **not** compatible with Python 3.13 or newer due to a dependency conflict.
- **Git**: For cloning the repository.

## Installation Steps

1.  **Clone the Repository**
    ```bash
    git clone <repository-url>
    cd PRISM
    ```

2.  **Create and Activate a Virtual Environment**
    It is highly recommended to install PRISM in a dedicated virtual environment.
    ```bash
    # Create the virtual environment
    python -m venv .venv

    # Activate it (on macOS/Linux)
    source .venv/bin/activate

    # Or on Windows
    .\venv\Scripts\activate
    ```

3.  **Install Dependencies**
    The project uses `pyproject.toml` to manage dependencies. Install the project in editable mode, which will also install all required packages.
    ```bash
    pip install -e .
    ```

4.  **Configure PRISM**
    Before you can use the `ask` command, you need to configure your preferred LLM provider.
    ```bash
    prism advanced configure
    ```
    This will prompt you to select a provider and enter your API key. 
    
    **💡 Tip:** For the quickest start, we recommend choosing the **OpenRouter** option. It's free and only requires a single API key to get started.

5.  **Initialize the Database (Optional but Recommended)**
    To save search results, you need to initialize the local SQLite database.
    ```bash
    prism advanced init
    ```
    The `search` command will also prompt you to do this automatically if you try to save results to an uninitialized database.

You are now ready to use PRISM!
