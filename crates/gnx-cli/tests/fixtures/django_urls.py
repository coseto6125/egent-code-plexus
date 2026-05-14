from django.urls import path
from . import views


def login_view(request):
    pass


def fallback_handler(request):
    pass


urlpatterns = [
    path("users/", views.user_list, name="user-list"),
    path("users/<int:id>/", views.user_detail, name="user-detail"),
    path("login/", login_view),
    path("fallback/", fallback_handler),
]

# Negative control: a `path()` call outside urlpatterns must not be captured.
import os
some_path = os.path("/tmp")
