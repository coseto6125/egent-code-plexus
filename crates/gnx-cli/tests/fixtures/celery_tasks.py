from celery import shared_task


@shared_task
def send_email(to: str, subject: str):
    pass


@app.task
def process_data(data):
    pass


@celery.task(bind=True)
def retry_job(self):
    pass


# Negative control: non-task decorators must not be captured.
@cached_property
def something():
    pass
