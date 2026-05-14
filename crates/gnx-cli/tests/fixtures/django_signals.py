from django.dispatch import receiver
from django.db.models.signals import post_save, pre_delete


def user_handler(sender, instance, **kwargs):
    pass


def order_handler(sender, instance, **kwargs):
    pass


def cleanup_handler(sender, instance, **kwargs):
    pass


# Pattern A: @receiver decorator
@receiver(post_save, sender="auth.User")
def saved_handler(sender, instance, **kwargs):
    pass


@receiver(pre_delete)
def deleted_handler(sender, instance, **kwargs):
    pass


# Pattern B: signal.connect() direct call
post_save.connect(user_handler, sender="auth.User")
post_save.connect(order_handler)
pre_delete.connect(cleanup_handler)

# Negative: lambda — should NOT be captured
post_save.connect(lambda sender, **kwargs: None)

# Negative: attribute access — should NOT be captured
post_save.connect(some_module.complex_handler)
