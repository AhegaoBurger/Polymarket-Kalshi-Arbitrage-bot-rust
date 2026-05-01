"""Entry point for `python -m ai_matcher`. Delegates to cli.main()."""

from ai_matcher.cli import main

if __name__ == "__main__":
    raise SystemExit(main())
