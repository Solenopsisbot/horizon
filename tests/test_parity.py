import re
import unittest
from pathlib import Path

README = Path(__file__).resolve().parents[1] / 'README.md'


class ParityTests(unittest.TestCase):
    def test_readme_scoped_status_is_explicit(self):
        text = README.read_text()
        self.assertIn('vanilla%20parity-scoped', text)
        self.assertIn('**Port-ledger checklist: 0 open** of 833 method-level rows.', text)
        self.assertIn('does not prove that the corresponding runtime is complete', text)



if __name__ == '__main__':
    unittest.main()
