[![Build Status](https://github.com/timmmm/rget/actions/workflows/build.yml/badge.svg)](https://github.com/Timmmm/rget/actions/workflows/build.yml)

# Rget

This is a very simple tool to download and optionally untar tarballs, but crucially it supports etag caching. Caches are stored in ~/.cache/rget.

## Installation

Download a binary release from [the Github releases page](https://github.com/Timmmm/rget/releases).

## PyPI Release Process

Make a release, download the wheels, then:

    python3 -m pip install twine
    python3 -m twine upload *.whl

Get the API key from here: https://pypi.org/manage/account/token/
