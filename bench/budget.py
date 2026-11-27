"""Observe the shared OpenRouter key; refuse unknown or exhausted budgets."""
from decimal import Decimal, InvalidOperation
import json
from pathlib import Path
import stat
import urllib.request


class BudgetStop(RuntimeError):
    pass


def amount(value):
    if isinstance(value, bool) or value is None:
        raise BudgetStop("OpenRouter usage is absent or invalid")
    try:
        result = Decimal(str(value))
    except InvalidOperation:
        raise BudgetStop("OpenRouter usage is invalid") from None
    if not result.is_finite() or result < 0:
        raise BudgetStop("OpenRouter usage is invalid")
    return result


def key_material(path):
    metadata = path.lstat()
    if not stat.S_ISREG(metadata.st_mode) or stat.S_IMODE(metadata.st_mode) != 0o600 or metadata.st_size > 4096:
        raise BudgetStop("OpenRouter key must be a private regular file within 4 KB")
    key = path.read_text().strip()
    if not key or any(char.isspace() for char in key):
        raise BudgetStop("OpenRouter key format is invalid")
    return key


def usage(path, credential=None):
    try:
        request = urllib.request.Request('https://openrouter.ai/api/v1/key',
                                         headers={'Authorization': 'Bearer ' + (credential if credential is not None else key_material(path))})
        with urllib.request.urlopen(request, timeout=5) as response:
            body = response.read(65537)
        if len(body) > 65536:
            raise BudgetStop("OpenRouter usage response exceeded its bound")
        data = json.loads(body)['data']
        return {field: float(amount(data.get(field))) for field in ['usage', 'usage_weekly']}
    except (OSError, ValueError, KeyError, TypeError):
        # Response bodies, header values and key labels never enter reports.
        raise BudgetStop("OpenRouter usage could not be verified; paid execution stopped") from None


class Budget:
    def __init__(self, key, observer=usage):
        self.key = Path(key)
        self.observe = observer
        self.start = None
        self.stopped = False
        self.last = None
        self.credential = None

    def check(self):
        if self.stopped:
            raise BudgetStop("OpenRouter run budget already stopped")
        try:
            selected = key_material(self.key)
            if self.credential is None:
                self.credential = selected
            if selected != self.credential:
                raise BudgetStop('OpenRouter key changed during this run; paid execution stopped')
            current = (usage(self.key, credential=self.credential) if self.observe is usage
                       else self.observe(self.key))
            total, week = amount(current['usage']), amount(current['usage_weekly'])
            if self.start is None:
                self.start = total
            if total < self.start:
                raise BudgetStop("OpenRouter cumulative usage moved backwards")
            result = dict(current, run_shared_key_usage_usd=float(total-self.start))
            self.last = result
            if total-self.start >= 30 or week >= 70:
                self.stopped = True
                raise BudgetStop("OpenRouter $30 run or $70 UTC-week stop reached")
            return result
        except BudgetStop:
            self.stopped = True
            raise
        except (OSError, ValueError, KeyError, TypeError):
            self.stopped = True
            raise BudgetStop('OpenRouter usage could not be verified; paid execution stopped') from None
