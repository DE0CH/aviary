# Installed into the sandbox venv as sbx_reportlab_cjk (loaded by sbx-reportlab-cjk.pth at every Python start).
# reportlab reads only TrueType outlines, but the image's Noto CJK fonts (fonts-noto-cjk) are OpenType/CFF .ttc files,
# and Claude takes its font path from fc-list, which lists those: TTFont() then fails with "postscript outlines are not
# supported" (live chat 2026-10-22). Once reportlab.pdfbase.ttfonts is imported, TTFont() given a Noto Sans/Serif CJK
# file gets the matching TrueType Noto Sans HK / Noto Serif HK (Regular, or Bold for Medium and heavier) instead.
import importlib.abc
import importlib.machinery
import os
import re
import sys

_TTF = '/usr/share/fonts/truetype/noto-hk'
_CJK = re.compile(r'Noto(Sans|Serif)(?:Mono)?CJK\w*-(\w+)\.(?:ttc|otf)$', re.I)


def _truetype(filename):
    m = _CJK.search(os.path.basename(str(filename)))
    if not m:
        return None
    weight = 'Bold' if m.group(2).lower() in ('medium', 'semibold', 'bold', 'black', 'heavy') else 'Regular'
    path = f'{_TTF}/Noto{m.group(1).capitalize()}HK-{weight}.ttf'
    return path if os.path.exists(path) else None


def _patch(ttfonts):
    init = ttfonts.TTFont.__init__

    def __init__(self, name, filename, *args, **kwargs):
        ttf = _truetype(filename)
        if ttf:
            filename = ttf
            kwargs.pop('subfontIndex', None)
        init(self, name, filename, *args, **kwargs)

    ttfonts.TTFont.__init__ = __init__


class _Finder(importlib.abc.MetaPathFinder):
    def find_spec(self, fullname, path, target=None):
        if fullname != 'reportlab.pdfbase.ttfonts':
            return None
        spec = importlib.machinery.PathFinder.find_spec(fullname, path)
        if spec is None or spec.loader is None:
            return spec
        exec_module = spec.loader.exec_module

        def patched(module):
            exec_module(module)
            _patch(module)

        spec.loader.exec_module = patched
        return spec


sys.meta_path.insert(0, _Finder())
