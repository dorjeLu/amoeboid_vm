import os

from hypothesis import settings

settings.register_profile("default", max_examples=200)
settings.register_profile("thorough", max_examples=5000)
settings.load_profile(os.environ.get("HYPOTHESIS_PROFILE", "default"))
